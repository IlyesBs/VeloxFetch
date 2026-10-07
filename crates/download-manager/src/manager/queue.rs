//! Queue control, the time-window scheduler and the (opt-in) power action.
//!
//! ```text
//!  user pause / schedule window ──► gate ──► SlotScheduler (max active, priority order)
//!  everything finished ──► [armed?] ──► visible countdown ──► SystemIntegration::power (once, then disarmed)
//! ```
//! Design rules that keep this from regressing the rest of the manager:
//! * The gate is *open* by default and a disabled schedule never closes it, so behaviour is
//!   identical to before unless the user turns something on.
//! * Closing the gate never touches downloads that are merely waiting; they keep their place in
//!   the queue. Only downloads holding a run slot are paused, and only when asked to be.
//! * Downloads paused by the queue are remembered (`held`, persisted) and resumed when it reopens;
//!   downloads the user paused themselves are never resumed by the queue.
//! * A power action is armed only by an explicit request, is in-memory only (a restart disarms it),
//!   fires once, and is preceded by a cancellable countdown.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use scheduler::Schedule;

use super::*;

const MAX_ACTIVE_KEY: &str = "queue.max_active";
const MAX_CONNECTIONS_KEY: &str = "queue.max_connections";
const PAUSED_KEY: &str = "queue.paused";
const HELD_KEY: &str = "queue.held";
const SCHEDULE_KEY: &str = "queue.schedule";
const KEEP_AWAKE_KEY: &str = "power.keep_awake";

const MIN_LIMIT: u32 = 1;
/// Downloads running at once.
const MAX_LIMIT: u32 = 32;
/// Connections per download (the engine's own hard ceiling).
const MAX_CONNECTIONS_LIMIT: u32 = downloader_core::planner::MAX_CONNECTIONS as u32;

#[derive(Default)]
pub(super) struct PowerRuntime {
    action: Option<PowerAction>,
    /// A download started or was running since arming (otherwise "nothing running" means nothing).
    saw_activity: bool,
    /// The user paused a download since arming: the queue is not really finished.
    blocked_by_pause: bool,
    deadline: Option<Instant>,
    deadline_ms: Option<u64>,
    last_error: Option<String>,
}

pub(super) struct QueueState {
    max_active: usize,
    max_connections: usize,
    manual_paused: bool,
    /// Downloads paused by the queue or schedule, to resume when it reopens.
    held: Vec<i64>,
    schedule: Schedule,
    /// Gate state last applied, `None` before the first evaluation.
    applied_open: Option<bool>,
    power: PowerRuntime,
}

impl QueueState {
    /// Saved values win over the configuration defaults. Corrupt values are ignored, not fatal.
    pub(super) fn load(store: &SqliteStore, cfg: &ManagerConfig) -> Result<Self, DownloadError> {
        let num = |key: &str, max: u32| -> Result<Option<usize>, DownloadError> {
            Ok(store.get_setting(key)?.and_then(|s| s.parse::<u32>().ok()).filter(|v| (MIN_LIMIT..=max).contains(v)).map(|v| v as usize))
        };
        let schedule = store
            .get_setting(SCHEDULE_KEY)?
            .and_then(|s| serde_json::from_str::<Schedule>(&s).ok())
            .filter(|s| s.validate().is_ok())
            .unwrap_or_default();
        let held = store.get_setting(HELD_KEY)?.map(|s| s.split(',').filter_map(|p| p.parse::<i64>().ok()).collect()).unwrap_or_default();
        Ok(Self {
            max_active: num(MAX_ACTIVE_KEY, MAX_LIMIT)?.unwrap_or(cfg.max_concurrent_downloads.max(1)),
            max_connections: num(MAX_CONNECTIONS_KEY, MAX_CONNECTIONS_LIMIT)?
                .unwrap_or(cfg.download.max_connections.clamp(1, MAX_CONNECTIONS_LIMIT as usize)),
            manual_paused: store.get_setting(PAUSED_KEY)?.as_deref() == Some("1"),
            held,
            schedule,
            applied_open: None,
            power: PowerRuntime::default(),
        })
    }

    pub(super) fn max_active(&self) -> usize {
        self.max_active
    }

    pub(super) fn max_connections(&self) -> usize {
        self.max_connections
    }

    pub(super) fn wants_open(&self, now: scheduler::chrono::NaiveDateTime) -> bool {
        !self.manual_paused && self.schedule.is_open(now)
    }

    /// Note that downloads are (or were just) active while a power action is armed.
    pub(super) fn note_activity(&mut self) {
        if self.power.action.is_some() {
            self.power.saw_activity = true;
        }
    }

    pub(super) fn note_user_pause(&mut self) {
        self.power.blocked_by_pause = true;
    }

    pub(super) fn note_user_start(&mut self) {
        self.power.blocked_by_pause = false;
    }
}

fn unix_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

impl DownloadManager {
    // ------------------------------------------------------------------ public API

    pub async fn get_queue_state(&self) -> Result<QueueStateDto, ManagerError> {
        Ok(self.queue_dto())
    }

    /// Change how many downloads run at once and how many connections each may use.
    pub async fn set_queue_config(&self, req: SetQueueConfig) -> Result<QueueStateDto, ManagerError> {
        let check = |name: &str, v: u32, max: u32| {
            if (MIN_LIMIT..=max).contains(&v) {
                Ok(v as usize)
            } else {
                Err(ManagerError::invalid(format!("{name} must be between {MIN_LIMIT} and {max}")))
            }
        };
        let max_active = req.max_active.map(|v| check("max active downloads", v, MAX_LIMIT)).transpose()?;
        let max_connections = req.max_connections.map(|v| check("max connections per download", v, MAX_CONNECTIONS_LIMIT)).transpose()?;
        if let Some(n) = max_active {
            self.inner.scheduler.set_max_slots(n);
            self.inner.queue.lock().unwrap().max_active = n;
            self.save_text(MAX_ACTIVE_KEY, Some(n.to_string())).await?;
        }
        if let Some(n) = max_connections {
            self.inner.downloader.set_max_connections(n);
            self.inner.queue.lock().unwrap().max_connections = n;
            self.save_text(MAX_CONNECTIONS_KEY, Some(n.to_string())).await?;
        }
        self.emit_queue();
        Ok(self.queue_dto())
    }

    /// Stop starting new downloads. With `pause_running` the ones running now are paused too and
    /// come back on [`resume_queue`](Self::resume_queue).
    pub async fn pause_queue(&self, req: PauseQueue) -> Result<QueueStateDto, ManagerError> {
        self.inner.queue.lock().unwrap().manual_paused = true;
        self.save_text(PAUSED_KEY, Some("1".into())).await?;
        self.reconcile().await;
        if req.pause_running {
            self.hold_running().await;
        }
        self.emit_queue();
        Ok(self.queue_dto())
    }

    pub async fn resume_queue(&self) -> Result<QueueStateDto, ManagerError> {
        self.inner.queue.lock().unwrap().manual_paused = false;
        self.save_text(PAUSED_KEY, None).await?;
        self.reconcile().await;
        self.emit_queue();
        Ok(self.queue_dto())
    }

    /// Replace the schedule ("start at 22:00, stop at 07:00"). Applied immediately: enabling it
    /// outside the window pauses running downloads until the window opens.
    pub async fn set_schedule(&self, schedule: Schedule) -> Result<QueueStateDto, ManagerError> {
        schedule.validate().map_err(|e| ManagerError::invalid(e.to_string()))?;
        let json = serde_json::to_string(&schedule).map_err(|e| ManagerError::Internal(e.to_string()))?;
        self.save_text(SCHEDULE_KEY, Some(json)).await?;
        self.inner.queue.lock().unwrap().schedule = schedule;
        self.reconcile().await;
        self.emit_queue();
        Ok(self.queue_dto())
    }

    /// Arm (`Some`) or disarm (`None`, which also cancels a running countdown) the one-shot power
    /// action that runs after every download has finished.
    pub async fn set_power_action(&self, req: SetPowerAction) -> Result<QueueStateDto, ManagerError> {
        if let Some(a) = req.action {
            if !self.inner.cfg.system.0.power_actions().contains(&a) {
                return Err(ManagerError::invalid(format!("{a:?} is not supported on this system")));
            }
        }
        let busy = !self.inner.active.lock().unwrap().is_empty() || self.media_busy();
        {
            let mut q = self.inner.queue.lock().unwrap();
            q.power = PowerRuntime { action: req.action, saw_activity: busy && req.action.is_some(), ..Default::default() };
        }
        self.emit_queue();
        Ok(self.queue_dto())
    }

    // ------------------------------------------------------------------ evaluation loop

    pub(super) fn spawn_queue_loop(&self) {
        let weak = Arc::downgrade(&self.inner);
        let every = self.inner.cfg.queue_poll;
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(every);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tick.tick().await;
                let Some(inner) = weak.upgrade() else { return };
                let mgr = DownloadManager { inner };
                mgr.reconcile().await;
                mgr.evaluate_queues().await;
                mgr.update_keep_awake();
                mgr.check_power().await;
            }
        });
    }

    fn now(&self) -> scheduler::chrono::NaiveDateTime {
        (self.inner.cfg.clock.0)()
    }

    /// Bring the run-slot gate in line with "not paused and inside the window", and act on a change.
    pub(super) async fn reconcile(&self) {
        if self.inner.shutting_down.load(Ordering::Relaxed) {
            return;
        }
        let _serial = self.inner.queue_lock.lock().await;
        let now = self.now();
        let (want_open, prev, schedule_closed, schedule_enabled, has_held) = {
            let mut q = self.inner.queue.lock().unwrap();
            let want = q.wants_open(now);
            let prev = q.applied_open;
            if prev == Some(want) {
                return;
            }
            q.applied_open = Some(want);
            (want, prev, !q.schedule.is_open(now), q.schedule.enabled, !q.held.is_empty())
        };
        if want_open {
            // First evaluation after launch only acts if there is something the queue owes us.
            // Resumed downloads re-enter the queue while the gate is still closed, at the front, and
            // only then does the gate open: otherwise an older waiting download would grab the free
            // slot first and the one we just resumed would queue up behind it.
            if prev.is_some() || schedule_enabled || has_held {
                self.release_held(schedule_enabled).await;
            }
            self.inner.scheduler.set_paused(false);
        } else {
            self.inner.scheduler.set_paused(true);
            if schedule_closed && prev != Some(false) {
                self.hold_running().await; // "Stop at 07:00": running downloads pause until the window reopens
            }
        }
        if prev.is_some() || !want_open {
            self.emit_queue();
        }
    }

    /// Pause the downloads that hold a run slot and remember them. Downloads waiting for a slot are
    /// left alone: with the gate closed they simply wait, in order.
    async fn hold_running(&self) {
        let targets: Vec<(i64, DownloadControl, watch::Receiver<bool>)> = {
            let active = self.inner.active.lock().unwrap();
            active
                .iter()
                .filter(|(_, a)| a.progress.queue_key().is_some_and(|k| !self.inner.scheduler.is_waiting(k)))
                .map(|(id, a)| (*id, a.control.clone(), a.done.clone()))
                .collect()
        };
        for (_, control, _) in &targets {
            control.pause();
        }
        let mut paused = Vec::new();
        for (id, _, mut done) in targets {
            let _ = tokio::time::timeout(Duration::from_secs(15), done.wait_for(|d| *d)).await;
            if matches!(self.load(id).await, Ok(rec) if rec.status == DownloadStatus::Paused) {
                paused.push(id);
            }
        }
        if paused.is_empty() {
            return;
        }
        let csv = {
            let mut q = self.inner.queue.lock().unwrap();
            for id in paused {
                if !q.held.contains(&id) {
                    q.held.push(id);
                }
            }
            q.held.iter().map(i64::to_string).collect::<Vec<_>>().join(",")
        };
        let _ = self.save_text(HELD_KEY, Some(csv)).await;
    }

    /// Resume what the queue paused and, when a schedule is in use, start downloads that were
    /// queued but never started (e.g. added while the window was closed, before a restart).
    async fn release_held(&self, start_queued: bool) {
        let held = std::mem::take(&mut self.inner.queue.lock().unwrap().held);
        if !held.is_empty() {
            let _ = self.save_text(HELD_KEY, None).await;
        }
        let mut requeued = Vec::new();
        for id in held {
            // Only what is still paused: the user may have cancelled or deleted it meanwhile.
            if matches!(self.load(id).await, Ok(rec) if rec.status == DownloadStatus::Paused) {
                self.inner.scheduler.prefer(id as u64); // engine keys are download ids
                if self.start_inner(id).await.is_ok() {
                    requeued.push(id as u64);
                }
            }
        }
        // Wait (briefly) until they have actually joined the queue, so the caller can open the gate.
        let t = Instant::now();
        while t.elapsed() < Duration::from_millis(1500)
            && !requeued.iter().all(|k| self.inner.scheduler.is_waiting(*k) || !self.is_active(*k as i64))
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        if start_queued {
            let store = self.inner.store.clone();
            let queued: Vec<i64> = blocking(move || store.list())
                .await
                .unwrap_or_default()
                .into_iter()
                .filter(|r| r.status == DownloadStatus::Queued)
                .filter_map(|r| r.id)
                .collect();
            for id in queued {
                if !self.is_active(id) {
                    let _ = self.start_inner(id).await;
                }
            }
        }
    }

    // ------------------------------------------------------------------ keep awake

    /// Keep the computer from sleeping on its own while downloads run (on by default).
    pub fn keep_awake_enabled(&self) -> bool {
        self.inner.store.get_setting(KEEP_AWAKE_KEY).ok().flatten().is_none_or(|v| v != "false")
    }

    pub async fn set_keep_awake(&self, on: bool) -> Result<QueueStateDto, ManagerError> {
        self.save_text(KEEP_AWAKE_KEY, Some(if on { "true" } else { "false" }.into())).await?;
        self.inner.awake_unavailable.store(false, Ordering::Relaxed); // changed: try again
        self.update_keep_awake();
        self.emit_queue();
        Ok(self.queue_dto())
    }

    /// Hold the computer awake exactly while something downloads (and the user wants it). Released before a
    /// power action can run: by then nothing downloads.
    pub(super) fn update_keep_awake(&self) {
        let busy =
            !self.inner.shutting_down.load(Ordering::Relaxed) && (!self.inner.active.lock().unwrap().is_empty() || self.media_busy());
        let want = busy && self.keep_awake_enabled() && !self.inner.awake_unavailable.load(Ordering::Relaxed);
        // No event: ordinary downloads never produce queue events (the UI shows "kept awake" from the live
        // download count it already receives).
        let mut slot = self.inner.awake.lock().unwrap();
        match (want, slot.is_some()) {
            (true, false) => match self.inner.cfg.system.0.keep_awake("Downloading files") {
                Ok(guard) => *slot = Some(guard),
                Err(e) => {
                    tracing::warn!(error = %e, "cannot keep the computer awake while downloading");
                    self.inner.awake_unavailable.store(true, Ordering::Relaxed);
                }
            },
            (false, true) => *slot = None,
            _ => {}
        }
    }

    // ------------------------------------------------------------------ power

    pub(super) async fn check_power(&self) {
        if self.inner.shutting_down.load(Ordering::Relaxed) {
            return;
        }
        let busy = !self.inner.active.lock().unwrap().is_empty() || self.media_busy();
        enum Next {
            Nothing,
            StartCountdown,
            Abort,
            Fire(PowerAction),
        }
        let next = {
            let mut q = self.inner.queue.lock().unwrap();
            let Some(action) = q.power.action else { return };
            let drained = !busy && q.held.is_empty() && q.power.saw_activity && !q.power.blocked_by_pause;
            match (q.power.deadline, drained) {
                (None, true) => {
                    let grace = self.inner.cfg.power_grace;
                    q.power.deadline = Some(Instant::now() + grace);
                    q.power.deadline_ms = Some(unix_ms() + grace.as_millis() as u64);
                    Next::StartCountdown
                }
                (Some(_), false) => {
                    q.power.deadline = None;
                    q.power.deadline_ms = None;
                    Next::Abort
                }
                (Some(at), true) if Instant::now() >= at => {
                    q.power = PowerRuntime::default(); // one-shot: disarm before acting
                    Next::Fire(action)
                }
                _ => Next::Nothing,
            }
        };
        match next {
            Next::Nothing => {}
            Next::StartCountdown | Next::Abort => self.emit_queue(),
            Next::Fire(action) => {
                tracing::warn!(?action, "all downloads finished; running the power action that was armed");
                let system = self.inner.cfg.system.0.clone();
                let res = tokio::task::spawn_blocking(move || system.power(action)).await;
                let err = match res {
                    Ok(Ok(())) => None,
                    Ok(Err(e)) => Some(e.to_string()),
                    Err(e) => Some(e.to_string()),
                };
                self.inner.queue.lock().unwrap().power.last_error = err;
                self.emit_queue();
            }
        }
    }

    // ------------------------------------------------------------------ state for the UI

    pub(super) fn emit_queue(&self) {
        let _ = self.inner.events.send(ManagerEvent::Queue { queue: self.queue_dto() });
    }

    fn queue_dto(&self) -> QueueStateDto {
        let now = self.now();
        let q = self.inner.queue.lock().unwrap();
        let phase = match (q.power.action, q.power.deadline) {
            (None, _) => PowerPhase::Off,
            (Some(_), None) => PowerPhase::Armed,
            (Some(_), Some(_)) => PowerPhase::Countdown,
        };
        QueueStateDto {
            max_active: q.max_active,
            max_connections: q.max_connections,
            paused: q.manual_paused,
            gate_open: !self.inner.scheduler.is_paused(),
            running: self.inner.scheduler.running(),
            waiting: self.inner.scheduler.queued(),
            held: q.held.len(),
            schedule: q.schedule.clone(),
            schedule_open: q.schedule.is_open(now),
            next_change: q.schedule.next_change(now).map(|t| t.format("%Y-%m-%dT%H:%M").to_string()),
            power: PowerStateDto {
                action: q.power.action,
                phase,
                countdown_ends_at_ms: q.power.deadline_ms,
                grace_secs: self.inner.cfg.power_grace.as_secs(),
                supported: self.inner.cfg.system.0.power_actions(),
                last_error: q.power.last_error.clone(),
                keep_awake: self.keep_awake_enabled(),
                kept_awake: self.inner.awake.lock().unwrap().is_some(),
                keep_awake_available: !self.inner.awake_unavailable.load(Ordering::Relaxed),
            },
        }
    }

    async fn save_text(&self, key: &str, value: Option<String>) -> Result<(), ManagerError> {
        let (store, key) = (self.inner.store.clone(), key.to_string());
        blocking(move || match value {
            Some(v) => store.set_setting(&key, &v),
            None => store.delete_setting(&key),
        })
        .await?;
        Ok(())
    }
}
