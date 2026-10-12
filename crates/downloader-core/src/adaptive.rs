//! Adaptive connection count.
//!
//! * [`AdaptiveController`]: pure decision logic. Each interval it sees throughput, error and
//!   latency and answers "use this many connections now". It doubles (16 -> 32 -> 64) while extra
//!   connections still pay off, and backs off (64 -> 32 -> 16) on errors or when throughput really
//!   collapses. New connections get a settling interval before they are judged (a fresh connection
//!   is still handshaking and in TCP slow start), and a speed limit in force freezes the decisions:
//!   a capped throughput says nothing about the network.
//! * Steady once settled: a server that answered 429 / 503 is not probed again for a long while (the wait
//!   doubles each time it says no again), a probe that brings nothing doubles the wait before the next one,
//!   and a speed that merely dips (the network, a busy server) never cuts the connections below
//!   [`COLLAPSE_FLOOR`]: with few connections, fewer only means slower. Only errors and explicit throttling go lower.
//! * [`AdaptiveGate`]: a concurrency limit that can be raised and lowered while in flight. After the server said
//!   "too many connections", it also holds back *new* requests for a while (see [`AdaptiveGate::server_refused`]):
//!   the connections already transferring go on, but no part asks again at once (one refusal freeing a slot for the
//!   next part, refused too, made dozens of refused requests in a row and seconds without a byte).

use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore};

/// A falling speed alone (no errors, no throttling answer) steps down only while more connections than this run.
pub const COLLAPSE_FLOOR: usize = 4;
/// The wait before probing upward again doubles at most this many times (15 s -> 8 min with the defaults).
const MAX_BACKOFF: u32 = 5;
/// While holding, the best speed seen fades by this much per interval, so one lucky burst does not stand as the
/// reference for ever.
const BEST_FADE: f64 = 0.995;
/// After a "too many connections" answer, new requests wait this long; each refusal in a row doubles it, up to
/// [`REFUSED_MAX_WAIT`]. A request the server accepts starts the series again.
pub const REFUSED_WAIT: Duration = Duration::from_secs(1);
pub const REFUSED_MAX_WAIT: Duration = Duration::from_secs(16);
/// A refusal after the connection count has worked this long without one is a passing hiccup: new requests wait
/// (the gate's hold) but the count stays (halving 2 -> 1 for one refusal halved the speed for minutes).
pub const STABLE_FOR: Duration = Duration::from_secs(20);

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
    /// Times the server said "too many connections" (429 / 503) during this run.
    throttle_hits: u32,
    /// Probes upward in a row that brought nothing.
    probe_fails: u32,
    /// When `limit` last changed.
    changed_at: Instant,
    /// The last "too many connections" answer, lowering or not.
    last_refusal: Option<Instant>,
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
            throttle_hits: 0,
            probe_fails: 0,
            changed_at: Instant::now(),
            last_refusal: None,
        }
    }

    pub fn limit(&self) -> usize {
        self.limit
    }

    /// The most connections the user's settings allow now (a profile or Settings changed while the download runs).
    /// Lowering takes the limit down at once (the connections over it give way, nothing restarts); raising lets the
    /// controller probe up to it again, still only while more connections bring more speed. `Some(limit)` when the
    /// limit itself changed.
    pub fn set_ceiling(&mut self, ceiling: usize) -> Option<usize> {
        let ceiling = ceiling.max(1);
        if ceiling == self.ceiling {
            return None;
        }
        let raised = ceiling > self.ceiling;
        self.ceiling = ceiling;
        if raised {
            self.soft_ceiling = ceiling;
            if self.phase == Phase::Holding && self.cooldown > 1 {
                self.cooldown = 1; // probe soon, not after a long backoff
            }
            return None;
        }
        self.soft_ceiling = self.soft_ceiling.min(ceiling);
        (self.limit > ceiling).then(|| {
            self.limit = ceiling;
            self.changed_at = Instant::now();
            ceiling
        })
    }

    fn step_down(&mut self) -> usize {
        (self.limit / 2).max(1)
    }

    fn hold(&mut self) {
        self.phase = Phase::Holding;
        // Each "no" (a throttling server, a probe that brought nothing) doubles the wait before the next probe.
        let backoff = (self.throttle_hits + self.probe_fails).min(MAX_BACKOFF);
        self.cooldown = self.cfg.cooldown_intervals.saturating_mul(1 << backoff);
        self.raised = false;
        self.settle = 0;
        self.low_streak = 0;
    }

    /// Multiply the limit (up to `cap`) and give the new connections time to settle before judging them.
    fn raise(&mut self, throughput: f64, cap: usize, factor: usize) -> Option<usize> {
        self.prev_limit = self.limit;
        self.prev_throughput = throughput;
        self.limit = (self.limit * factor.max(2)).min(cap);
        self.changed_at = Instant::now();
        self.raised = true;
        self.settle = self.cfg.settle_intervals;
        Some(self.limit)
    }

    fn lower_to(&mut self, new: usize, throughput: f64) -> Option<usize> {
        self.probed = true;
        self.soft_ceiling = new; // do not immediately climb back into the trouble
        self.limit = new;
        self.changed_at = Instant::now();
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
        if self.last_throttle_drop.is_some_and(|t| now.saturating_duration_since(t) < self.cfg.interval) {
            return None;
        }
        // Not right after a change (a probe, the start), and the first refusal in a long while: a hiccup, keep the count.
        let hiccup = !self.raised
            && now.saturating_duration_since(self.changed_at) >= STABLE_FOR
            && self.last_refusal.is_none_or(|t| now.saturating_duration_since(t) >= STABLE_FOR);
        self.last_refusal = Some(now);
        self.last_throttle_drop = Some(now);
        if hiccup {
            return None;
        }
        self.throttle_hits = self.throttle_hits.saturating_add(1);
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
        let congested = slow && collapsed && !s.limited && self.limit > COLLAPSE_FLOOR;
        // Retries right after a "too many connections" answer are those answers: `on_throttled` already halved the
        // connections for them; counting them again here would halve twice (4 -> 2 -> 1 for a server allowing 2).
        let throttled_now = self.last_throttle_drop.is_some_and(|t| t.elapsed() < self.cfg.interval * 2);
        let failing = s.errors >= self.cfg.error_threshold && !throttled_now;
        if (failing || congested) && self.limit > 1 {
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
                    if self.probed {
                        self.probe_fails = self.probe_fails.saturating_add(1);
                    }
                    let back = self.prev_limit;
                    self.limit = back;
                    self.changed_at = Instant::now();
                    self.soft_ceiling = back;
                    self.best_throughput = self.prev_throughput.max(s.throughput);
                    self.hold();
                    return Some(back);
                }
                if self.raised {
                    self.probe_fails = 0; // it paid off
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
                    self.best_throughput = (self.best_throughput * BEST_FADE).max(s.throughput);
                }
                if self.low_streak >= self.cfg.degrade_intervals && self.limit > COLLAPSE_FLOOR {
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
/// are destroyed instead of returned. Which connections stop at once depends on who lowered it (see
/// [`AdaptiveGate::set_limit`] and [`AdaptiveGate::set_limit_soft`]): a request still waiting for the server's answer
/// always gives way first, a connection that is transferring only for a speed limit or the user's settings.
pub struct AdaptiveGate {
    sem: Arc<Semaphore>,
    limit: AtomicUsize,
    /// Connections that are transferring may go on while no more than this many are open (never below `limit`):
    /// raised with the limit, lowered only by [`AdaptiveGate::set_limit`].
    keep: AtomicUsize,
    /// The limit went down (wakes requests still waiting for an answer: see [`GatePermit::until_over_limit`]).
    lowered: Notify,
    pending_forget: AtomicUsize,
    active: AtomicUsize,
    peak: AtomicUsize,
    /// The next first-connection turn allowed to queue (see [`AdaptiveGate::acquire_in_turn`]).
    next_turn: AtomicUsize,
    turn_taken: Notify,
    /// No new request before this (the server refused one: see [`AdaptiveGate::server_refused`]).
    hold_until: Mutex<Option<tokio::time::Instant>>,
    /// Refusals in a row (reset by an accepted request).
    refusals: AtomicU32,
    /// Connections that are stopping because the limit went down (their slot not given back yet).
    shedding: AtomicUsize,
    /// Under a speed limit: the most one request asks for (0 = no limit, see [`AdaptiveGate::set_request_cap`]).
    request_cap: std::sync::atomic::AtomicU64,
    /// Parts waiting for a slot again, by rank (see [`AdaptiveGate::acquire_ranked`]); the second number keeps equal
    /// ranks apart.
    ranked: Mutex<std::collections::BTreeSet<(usize, u64)>>,
    ticket: std::sync::atomic::AtomicU64,
    /// The first of `ranked` changed.
    head_changed: Notify,
}

pub struct GatePermit {
    permit: Option<OwnedSemaphorePermit>,
    gate: Arc<AdaptiveGate>,
    /// This connection stops: one too many for the limit (see [`GatePermit::give_way`]).
    leaving: std::sync::atomic::AtomicBool,
}

impl AdaptiveGate {
    pub fn new(limit: usize) -> Arc<Self> {
        let limit = limit.max(1);
        Arc::new(Self {
            sem: Arc::new(Semaphore::new(limit)),
            limit: AtomicUsize::new(limit),
            keep: AtomicUsize::new(limit),
            lowered: Notify::new(),
            pending_forget: AtomicUsize::new(0),
            active: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            next_turn: AtomicUsize::new(0),
            turn_taken: Notify::new(),
            hold_until: Mutex::new(None),
            refusals: AtomicU32::new(0),
            shedding: AtomicUsize::new(0),
            request_cap: Default::default(),
            ranked: Mutex::default(),
            ticket: Default::default(),
            head_changed: Notify::new(),
        })
    }

    /// The server refused a request ("too many connections": HTTP 429 / 503). New requests of this download wait
    /// 1 s, 2 s, 4 s... while it keeps refusing (a shorter `Retry-After` is taken as said); the ones already
    /// transferring are not touched. Returns the wait.
    pub fn server_refused(&self, retry_after: Option<Duration>) -> Duration {
        let now = tokio::time::Instant::now();
        let mut h = self.hold_until.lock().unwrap_or_else(|p| p.into_inner());
        // Refusals of requests already on their way while a wait runs are the same "no" (a burst of them must not
        // make the wait grow to its longest at once); only a refusal after waiting counts as one more.
        if let Some(until) = h.filter(|until| *until > now) {
            return until - now;
        }
        let n = self.refusals.fetch_add(1, Ordering::SeqCst).min(8);
        let backoff = REFUSED_WAIT.saturating_mul(1 << n).min(REFUSED_MAX_WAIT);
        // A long `Retry-After` is for the refused request (its own retry honours it); holding every new request that
        // long would let the connections still running dwindle away one by one as they finish.
        let wait = retry_after.map_or(backoff, |ra| ra.clamp(REFUSED_WAIT, backoff));
        *h = Some(now + wait);
        wait
    }

    /// The server accepted a request: the next refusal waits the shortest time again.
    pub fn server_accepted(&self) {
        self.refusals.store(0, Ordering::SeqCst);
    }

    /// How long new requests still have to wait after a refusal (zero: none).
    pub fn held_for(&self) -> Duration {
        let h = *self.hold_until.lock().unwrap_or_else(|p| p.into_inner());
        h.map_or(Duration::ZERO, |t| t.saturating_duration_since(tokio::time::Instant::now()))
    }

    /// Wait out a refusal's hold (it may be extended while waiting). The caller holds its slot meanwhile, so when
    /// the hold ends, no more requests go than the limit allows.
    async fn wait_hold(&self) {
        loop {
            let left = self.held_for();
            if left.is_zero() {
                return;
            }
            tokio::time::sleep(left).await;
        }
    }

    pub fn limit(&self) -> usize {
        self.limit.load(Ordering::SeqCst)
    }

    /// Under a speed limit, each request asks for at most `bytes` and closes its connection when done (`None`: no
    /// limit, requests as usual). A connection that downloads slowly for long has the system grow its receive
    /// buffer all the same, read in big bites again; short requests on new connections keep it small.
    pub fn set_request_cap(&self, bytes: Option<u64>) {
        self.request_cap.store(bytes.unwrap_or(0), Ordering::SeqCst);
    }

    pub fn request_cap(&self) -> Option<u64> {
        Some(self.request_cap.load(Ordering::SeqCst)).filter(|b| *b > 0)
    }

    /// Connections in use right now.
    pub fn active(&self) -> usize {
        self.active.load(Ordering::SeqCst)
    }

    /// Most connections that were ever in use at the same time.
    pub fn peak_active(&self) -> usize {
        self.peak.load(Ordering::SeqCst)
    }

    /// A slot with no particular place (after every ranked part).
    pub async fn acquire(self: &Arc<Self>) -> GatePermit {
        self.acquire_ranked(usize::MAX).await
    }

    /// A segment's first connection: queue for a slot in plan order (`turn` 0, 1, 2, ...), whatever order the tasks
    /// happen to run in on the thread pool. The slots are first come, first served, so without this a loaded machine
    /// could let a later segment take the place of an earlier one (in preview order: the end of a video, which a
    /// player needs first, waiting behind the middle). Every turn from 0 must be taken once.
    pub async fn acquire_in_turn(self: &Arc<Self>, turn: usize) -> GatePermit {
        loop {
            // Created before the check: a turn taken in between still wakes it.
            let taken = self.turn_taken.notified();
            if self.next_turn.load(Ordering::SeqCst) >= turn {
                break;
            }
            taken.await;
        }
        // Queued among the parts by rank (its turn); only then the next turn may queue.
        self.ranked_after(turn, || {
            self.next_turn.fetch_max(turn + 1, Ordering::SeqCst);
            self.turn_taken.notify_waiters();
        })
        .await
    }

    /// A part queuing for a slot again (the first try queues with [`acquire_in_turn`](Self::acquire_in_turn)):
    /// slots go to the waiting part of the lowest `rank` (its plan order), not to whoever queued first. So a part
    /// that had to wait (another connection's run had it, its connection gave way, it switched to short requests)
    /// keeps its place: the start of a video being watched never waits behind the middle of the file.
    pub async fn acquire_ranked(self: &Arc<Self>, rank: usize) -> GatePermit {
        self.ranked_after(rank, || {}).await
    }

    /// [`acquire_ranked`](Self::acquire_ranked), calling `queued` once the place is taken. Every slot of the gate is
    /// handed out here, so only the first in line ever waits on the semaphore.
    async fn ranked_after(self: &Arc<Self>, rank: usize, queued: impl FnOnce()) -> GatePermit {
        let me = (rank, self.ticket.fetch_add(1, Ordering::Relaxed));
        let first = {
            let mut q = self.ranked_queue();
            q.insert(me);
            q.first() == Some(&me)
        };
        queued();
        if first {
            self.head_changed.notify_waiters();
        }
        /// Leaves the queue however the wait ends (a slot, or cancelled).
        struct Leave<'a>(&'a AdaptiveGate, (usize, u64));
        impl Drop for Leave<'_> {
            fn drop(&mut self) {
                let mut q = self.0.ranked_queue();
                let was_first = q.first() == Some(&self.1);
                q.remove(&self.1);
                drop(q);
                if was_first {
                    self.0.head_changed.notify_waiters();
                }
            }
        }
        let leave = Leave(self, me);
        let permit = loop {
            let changed = self.head_changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let first = self.ranked_queue().first() == Some(&me);
            if !first {
                changed.await;
                continue;
            }
            // First in line: wait for a slot, unless a lower rank arrives meanwhile.
            tokio::select! {
                biased;
                p = self.sem.clone().acquire_owned() => break p.expect("gate semaphore is never closed"),
                _ = &mut changed => {}
            }
        };
        drop(leave);
        self.wait_hold().await;
        let now = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(now, Ordering::SeqCst);
        GatePermit { permit: Some(permit), gate: self.clone(), leaving: Default::default() }
    }

    fn ranked_queue(&self) -> std::sync::MutexGuard<'_, std::collections::BTreeSet<(usize, u64)>> {
        self.ranked.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// A speed limit or the user's settings: the connections over the new limit stop where they are, transferring
    /// or not (a lower limit that waited for parts to finish could take minutes under a low speed limit).
    pub fn set_limit(&self, new: usize) {
        let new = new.max(1);
        self.keep.store(new, Ordering::SeqCst);
        self.resize(new);
    }

    /// The download's own judgement (more connections brought nothing, the speed dipped, errors, a server that
    /// refuses more): no new request over the limit, and requests still waiting for an answer give way, but a
    /// connection that is transferring finishes its request. On a slow server, cutting one throws away the bytes on
    /// their way and the time to get a new answer, which a single unbroken connection (a browser's) never loses.
    pub fn set_limit_soft(&self, new: usize) {
        let new = new.max(1);
        self.keep.fetch_max(new, Ordering::SeqCst);
        self.resize(new);
    }

    fn resize(&self, new: usize) {
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
            self.lowered.notify_waiters();
        }
    }
}

impl GatePermit {
    /// More connections are open than the limit now allows (it went down): `true` once, for as many connections as
    /// are one too many, which then stop where they are and give their slot back. Without this a lower limit would
    /// only apply as parts finish, which under a low speed limit can take minutes.
    /// [`give_way`](Self::give_way) picked this connection to stop.
    pub fn is_leaving(&self) -> bool {
        self.leaving.load(Ordering::SeqCst)
    }

    /// Before a kept slot makes its next request: wait out a hold after refusals, as a new slot would.
    pub async fn rest(&self) {
        self.gate.wait_hold().await;
    }

    /// Before a new request, or while one waits for the server's answer: is this connection one too many?
    pub fn give_way(&self) -> bool {
        self.over(&self.gate.limit)
    }

    /// While transferring: is this connection one too many for a limit that cuts transfers (see
    /// [`AdaptiveGate::set_limit`])? Requests still waiting for an answer leave first (they lose nothing).
    pub fn give_way_transferring(&self) -> bool {
        self.over(&self.gate.keep)
    }

    fn over(&self, limit: &AtomicUsize) -> bool {
        if self.leaving.load(Ordering::SeqCst) {
            return true;
        }
        let g = &self.gate;
        #[allow(deprecated)] // fetch_update = try_update on newer Rust; the old name builds everywhere
        let picked = g
            .shedding
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |leaving| {
                (g.active.load(Ordering::SeqCst) > limit.load(Ordering::SeqCst) + leaving).then_some(leaving + 1)
            })
            .is_ok();
        if picked {
            self.leaving.store(true, Ordering::SeqCst);
        }
        picked
    }

    /// Resolves once this connection is one too many for the limit ([`give_way`](Self::give_way)): a request still
    /// waiting for the server's answer stops waiting then (it has received nothing, so it loses nothing).
    pub async fn until_over_limit(&self) {
        loop {
            let lowered = self.gate.lowered.notified();
            tokio::pin!(lowered);
            lowered.as_mut().enable();
            if self.give_way() {
                return;
            }
            lowered.await;
        }
    }
}

impl Drop for GatePermit {
    fn drop(&mut self) {
        if self.leaving.load(Ordering::SeqCst) {
            self.gate.shedding.fetch_sub(1, Ordering::SeqCst);
        }
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn first_connections_get_their_slot_in_plan_order_whatever_order_the_tasks_run() {
        for round in 0..20 {
            let gate = AdaptiveGate::new(1);
            let order = Arc::new(std::sync::Mutex::new(Vec::new()));
            let mut set = tokio::task::JoinSet::new();
            // Spawned in reverse with staggered starts: the last turn's task runs first.
            for turn in (0..6usize).rev() {
                let (gate, order) = (gate.clone(), order.clone());
                set.spawn(async move {
                    tokio::time::sleep(Duration::from_millis(((6 - turn) * 2) as u64)).await;
                    let _p = gate.acquire_in_turn(turn).await;
                    order.lock().unwrap().push(turn);
                    tokio::time::sleep(Duration::from_millis(1)).await;
                });
            }
            while set.join_next().await.is_some() {}
            assert_eq!(*order.lock().unwrap(), (0..6).collect::<Vec<_>>(), "round {round}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_refusal_holds_new_requests_back_and_the_wait_grows_while_refused() {
        let g = AdaptiveGate::new(4);
        let running = g.acquire().await; // a transfer in progress: never touched
        assert_eq!(g.server_refused(None), Duration::from_secs(1));
        let t = tokio::time::Instant::now();
        let p = g.acquire().await;
        assert!(t.elapsed() >= Duration::from_secs(1), "a new request waited out the refusal");
        drop(p);
        // Refused again after waiting: twice as long. A burst of refusals during a wait is one refusal.
        let again = |g: &Arc<AdaptiveGate>, ra| {
            let w = g.server_refused(ra);
            for _ in 0..30 {
                g.server_refused(ra);
            }
            assert_eq!(g.held_for(), w, "the burst left the wait as it was");
            w
        };
        assert_eq!(again(&g, None), Duration::from_secs(2), "refused again: twice as long");
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert_eq!(again(&g, None), Duration::from_secs(4));
        for _ in 0..10 {
            tokio::time::sleep(g.held_for()).await;
            g.server_refused(None);
        }
        tokio::time::sleep(g.held_for()).await;
        assert_eq!(g.server_refused(None), REFUSED_MAX_WAIT, "never longer than the cap");
        tokio::time::sleep(REFUSED_MAX_WAIT).await;
        g.server_accepted();
        assert_eq!(g.server_refused(None), Duration::from_secs(1), "an accepted request starts the series again");
        tokio::time::sleep(Duration::from_secs(1)).await;
        g.server_accepted();
        assert_eq!(g.server_refused(Some(Duration::ZERO)), Duration::from_secs(1), "at least the shortest wait");
        tokio::time::sleep(Duration::from_secs(1)).await;
        g.server_accepted();
        assert_eq!(g.server_refused(Some(Duration::from_secs(30))), Duration::from_secs(1), "a long wait is the refused part's");
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert_eq!(g.server_refused(Some(Duration::from_millis(1500))), Duration::from_millis(1500), "a shorter one is taken");
        drop(running);
    }

    #[tokio::test(start_paused = true)]
    async fn after_a_refusal_only_the_limit_goes_when_the_hold_ends_not_a_storm() {
        // Limit 1, many parts waiting: the hold ends and exactly one request goes, not one after the other at once.
        let g = AdaptiveGate::new(1);
        g.server_refused(None);
        let went = Arc::new(AtomicUsize::new(0));
        let mut set = tokio::task::JoinSet::new();
        for turn in 0..20usize {
            let (g, went) = (g.clone(), went.clone());
            set.spawn(async move {
                let _p = g.acquire_in_turn(turn).await;
                went.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_secs(3600)).await; // the transfer
            });
        }
        tokio::time::sleep(Duration::from_millis(900)).await;
        assert_eq!(went.load(Ordering::SeqCst), 0, "nobody asks during the hold");
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(went.load(Ordering::SeqCst), 1, "one request when it ends");
        set.abort_all();
    }

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
    fn a_single_refusal_after_a_long_steady_run_keeps_the_connections() {
        let mut c = AdaptiveController::new(cfg(), 2, 2);
        let later = Instant::now() + STABLE_FOR + Duration::from_secs(5);
        assert_eq!(c.on_throttled(later), None, "2 worked for a long time: one refusal is a hiccup");
        assert_eq!(c.limit(), 2);
        assert_eq!(c.on_throttled(later + Duration::from_secs(5)), Some(1), "refused again soon: really too many");
        // Right after a probe upward, the first refusal already lowers.
        let mut p = AdaptiveController::new(cfg(), 2, 8);
        assert_eq!(p.observe(s(10.0)), Some(4));
        assert_eq!(p.on_throttled(Instant::now() + STABLE_FOR * 2), Some(2));
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

    #[test]
    fn a_throttling_answer_halves_once_not_twice() {
        // The server allows 2: three of four connections get a 429 at once. on_throttled halves to 2; the
        // sampler then sees those 3 retries as errors, which must not halve again to 1.
        let mut c = AdaptiveController::new(cfg(), 4, 64);
        assert_eq!(c.on_throttled(Instant::now()), Some(2));
        let after = Sample { throughput: 100.0, errors: 3, latency: None, limited: false };
        assert_eq!(c.observe(after), None);
        assert_eq!(c.limit(), 2);
        // Other failures (not throttling) still step down.
        let mut d = AdaptiveController::new(cfg(), 4, 64);
        assert_eq!(d.observe(after), Some(2));
    }

    #[test]
    fn a_dipping_speed_never_cuts_a_few_connections() {
        // 4 connections, then the line slows down for a long while (someone else uses it): fewer connections would
        // only be slower, so they stay. Above the floor, a lasting collapse still steps down (to the floor).
        let mut c = AdaptiveController::new(cfg(), 4, 4);
        assert_eq!(c.observe(s(100.0)), None);
        for _ in 0..20 {
            assert_eq!(c.observe(s(20.0)), None, "a dip is not a reason to drop connections at the floor");
        }
        assert_eq!(c.limit(), COLLAPSE_FLOOR);
        let mut big = AdaptiveController::new(cfg(), 16, 16);
        assert_eq!(big.observe(s(100.0)), None);
        assert_eq!(big.observe(s(20.0)), None);
        assert_eq!(big.observe(s(20.0)), Some(8));
    }

    #[test]
    fn a_throttling_server_is_probed_again_less_and_less_often() {
        // The server allows 2 connections: every probe to 4 gets a 429. The waits between probes double.
        let mut c = AdaptiveController::new(cfg(), 4, 64);
        let mut t = Instant::now();
        let mut gaps = Vec::new();
        assert_eq!(c.on_throttled(t), Some(2));
        let mut since = 0;
        for _ in 0..200 {
            since += 1;
            if let Some(n) = c.observe(s(tp(c.limit(), 2))) {
                if n > 2 {
                    gaps.push(since);
                    since = 0;
                    t += Duration::from_secs(5);
                    assert_eq!(c.on_throttled(t), Some(2), "the server says no again");
                }
            }
        }
        assert!(gaps.len() >= 3, "{gaps:?}");
        assert!(gaps.windows(2).all(|w| w[1] >= w[0] * 2 - 1), "each wait doubles: {gaps:?}");
    }

    #[test]
    fn useless_probes_back_off_and_a_useful_one_resets_the_wait() {
        let mut c = AdaptiveController::new(cfg(), 4, 64);
        // First climb: 4 -> 8 brings nothing (plateau at 4) -> back to 4.
        assert_eq!(c.observe(s(40.0)), Some(8));
        assert_eq!(c.observe(s(40.0)), Some(4));
        let mut gaps = Vec::new();
        let mut since = 0;
        for _ in 0..60 {
            since += 1;
            if c.observe(s(40.0)) == Some(8) {
                gaps.push(since);
                since = 0;
            }
        }
        assert!(gaps.len() >= 3 && gaps.windows(2).all(|w| w[1] > w[0]), "probes spread out: {gaps:?}");
    }

    #[tokio::test]
    async fn a_lower_limit_makes_exactly_the_extra_connections_give_way() {
        let g = AdaptiveGate::new(6);
        let held: Vec<GatePermit> = futures_util::future::join_all((0..6).map(|_| g.acquire())).await;
        assert!(held.iter().all(|p| !p.give_way()), "within the limit: nobody leaves");
        g.set_limit(2);
        let leaving: Vec<bool> = held.iter().map(|p| p.give_way()).collect();
        assert_eq!(leaving.iter().filter(|l| **l).count(), 4, "four of six stop: {leaving:?}");
        assert!(held.iter().zip(&leaving).all(|(p, l)| p.give_way() == *l), "the same ones, asked again");
        let mut held = held;
        let stay: Vec<GatePermit> = held.drain(..).zip(leaving).filter_map(|(p, l)| (!l).then_some(p)).collect();
        assert_eq!((g.active(), stay.len()), (2, 2));
        assert!(stay.iter().all(|p| !p.give_way()), "the limit is met: the others stay");
        assert!(tokio::time::timeout(Duration::from_millis(50), g.acquire()).await.is_err(), "no third slot");
    }

    #[tokio::test]
    async fn the_downloads_own_lower_limit_stops_new_and_waiting_requests_never_a_transfer() {
        // Issue #21: cutting a transfer on a slow server loses what is on its way and the wait for a new answer.
        let g = AdaptiveGate::new(6);
        let held: Vec<GatePermit> = futures_util::future::join_all((0..6).map(|_| g.acquire())).await;
        g.set_limit_soft(2);
        assert!(held.iter().all(|p| !p.give_way_transferring()), "every transfer goes on");
        let waiting: Vec<bool> = held.iter().map(|p| p.give_way()).collect();
        assert_eq!(waiting.iter().filter(|l| **l).count(), 4, "requests still waiting for an answer give way: {waiting:?}");
        let mut held = held;
        let stay: Vec<GatePermit> = held.drain(..).zip(waiting).filter_map(|(p, l)| (!l).then_some(p)).collect();
        assert_eq!(g.active(), 2);
        assert!(tokio::time::timeout(Duration::from_millis(50), g.acquire()).await.is_err(), "no new request over the limit");
        // A speed limit or the user's settings cut transfers too.
        g.set_limit(1);
        assert_eq!(stay.iter().filter(|p| p.give_way_transferring()).count(), 1);
        // Raised again: the transfers it allows are kept by a later lowering of its own.
        drop(stay);
        g.set_limit_soft(3);
        let three: Vec<GatePermit> = futures_util::future::join_all((0..3).map(|_| g.acquire())).await;
        g.set_limit_soft(1);
        assert!(three.iter().all(|p| !p.give_way_transferring()));
    }

    #[tokio::test]
    async fn a_request_waiting_for_its_answer_is_told_when_it_is_one_too_many() {
        let g = AdaptiveGate::new(2);
        let a = g.acquire().await;
        let b = g.acquire().await;
        let wait_b = tokio::spawn(async move {
            b.until_over_limit().await;
            b
        });
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(!wait_b.is_finished(), "within the limit: it keeps waiting");
        g.set_limit_soft(1);
        let b = tokio::time::timeout(Duration::from_secs(1), wait_b).await.expect("woken by the lower limit").unwrap();
        assert!(b.give_way());
        assert!(!a.give_way(), "only one of the two");
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

    #[tokio::test]
    async fn a_part_queuing_again_keeps_its_place() {
        let g = AdaptiveGate::new(1);
        let held = g.acquire_in_turn(0).await;
        // Later parts queue for their first try...
        let order = Arc::new(Mutex::new(Vec::new()));
        let mut waiting = Vec::new();
        for turn in 1..4 {
            let (g, order) = (g.clone(), order.clone());
            waiting.push(tokio::spawn(async move {
                let p = g.acquire_in_turn(turn).await;
                order.lock().unwrap().push(turn);
                tokio::time::sleep(Duration::from_millis(5)).await;
                drop(p);
            }));
            tokio::task::yield_now().await;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        // ...then part 0 has to queue again (another connection's run had it, or it gave way): it goes first.
        let again = {
            let (g, order) = (g.clone(), order.clone());
            tokio::spawn(async move {
                let p = g.acquire_ranked(0).await;
                order.lock().unwrap().push(0);
                drop(p);
            })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
        drop(held);
        again.await.unwrap();
        for w in waiting {
            w.await.unwrap();
        }
        assert_eq!(*order.lock().unwrap(), [0, 1, 2, 3], "the start first, then plan order");
    }

    #[tokio::test]
    async fn a_cancelled_wait_leaves_the_line() {
        let g = AdaptiveGate::new(1);
        let held = g.acquire().await;
        // The first in line gives up (its download was paused): the next one is not stuck behind it.
        assert!(tokio::time::timeout(Duration::from_millis(30), g.acquire_ranked(0)).await.is_err());
        let next = tokio::spawn({
            let g = g.clone();
            async move { g.acquire_ranked(5).await }
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        drop(held);
        assert!(tokio::time::timeout(Duration::from_secs(1), next).await.is_ok(), "the slot went to the next in line");
    }

    #[test]
    fn the_ceiling_follows_the_settings_while_running() {
        let mut c = AdaptiveController::new(AdaptiveConfig::default(), 8, 16);
        assert_eq!(c.set_ceiling(16), None, "unchanged");
        assert_eq!(c.set_ceiling(4), Some(4), "lowered below the limit: down at once");
        assert_eq!(c.limit(), 4);
        assert_eq!(c.set_ceiling(6), None, "raised: the limit stays, the controller may probe up to it");
        assert_eq!(c.limit(), 4);
        assert_eq!(c.set_ceiling(5), None, "lowered but still above the limit: nothing to shed");
        assert_eq!(c.set_ceiling(0), Some(1), "never below one connection");
    }
}
