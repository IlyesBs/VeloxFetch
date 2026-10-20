//! Smart Download Queue: several queues, each with a few plain rules, on top of the one run-slot scheduler.
//!
//! ```text
//!   every second ─► rules of each queue (time, network, connection, idle, busy line) ─► open / closed
//!        │                     closed: running downloads of the queue pause (remembered in `held`)
//!        │                     open again: they resume, and a queue with rules starts what was added for later
//!        ├─► the user's order + downloads of closed queues ─► SlotScheduler::set_policy (order, held back)
//!        ├─► "start at" times that came ─► those downloads start (their queue's rules do not hold them back)
//!        └─► a download bigger than a queue's "big files" size ─► moved to that queue
//! ```
//! Design rules that keep the rest of the manager as it was:
//! * The main queue (id 1) has no rule unless the user gives it one, so a user who never opens the queue page
//!   sees exactly the old behaviour: same slots, same priorities, same "Download later".
//! * The queues never touch the global pause, the global schedule window or the power action (`queue.rs`); both
//!   layers must agree before a download starts.
//! * Something the computer cannot tell (idle time, connection type, traffic) never holds a download back.
//! * Only downloads paused by a queue rule are resumed by it; a download the user paused stays paused.

use std::collections::HashSet;

use persistence::{Placement, QueueRow};
use scheduler::chrono::{Duration as Span, NaiveDateTime};
use scheduler::queue_rules::{Closed, Inputs, NetKind, RuleState};
use serde::{Deserialize, Serialize};
use system_integration::{ConnectionKind, NetworkState};

use super::*;

/// The main queue: every download that was not put anywhere else.
pub const MAIN_QUEUE: i64 = 1;
const DEFS_KEY: &str = "queues.list";
const STATE_KEY: &str = "queues.state";
const HELD_KEY: &str = "queues.held";
const MAX_QUEUES: usize = 16;
const MAX_NAME: usize = 40;
/// Downloads with no place of their own sort by when they were added (`id * GAP`), leaving room to move.
const GAP: i64 = 1024;
/// How often the network is looked at, in queue evaluations (about every 5 s).
const NET_EVERY_POLLS: u32 = 5;
/// "No network" readings in a row before downloads stop being started.
const OFFLINE_CONFIRM: u8 = 2;
/// How far the timeline looks ahead.
const HORIZON_HOURS: i64 = 24;
/// Smoothing of the traffic reading (weight of the newest one).
const TRAFFIC_EMA: f64 = 0.5;
/// A big file is moved to its queue only while little of it is done.
const ROUTE_BEFORE_SHARE: u64 = 4;
pub(super) const TIME_FMT: &str = "%Y-%m-%dT%H:%M";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct QueueDef {
    id: i64,
    name: String,
    rules: QueueRules,
}

/// What survives a restart besides the rules.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct Saved {
    paused: bool,
    forced: bool,
    used_until: Option<String>,
}

#[derive(Default)]
struct Runtime {
    paused: bool,
    forced: bool,
    rule: RuleState,
    gate: Option<Closed>,
    /// Open or closed as last applied (`None` before the first evaluation).
    applied_open: Option<bool>,
    other_bps: Option<f64>,
}

impl Runtime {
    fn open(&self) -> bool {
        self.gate.is_none()
    }
}

pub(super) struct Queues {
    defs: Vec<QueueDef>,
    rt: HashMap<i64, Runtime>,
    places: HashMap<i64, Placement>,
    /// Downloads paused by a queue rule, resumed when their queue opens again.
    held: Vec<i64>,
    /// "Start now" (or a start time that came): the queue's rules do not hold these back.
    forced: HashSet<i64>,
    net: NetworkState,
    net_at: Option<Instant>,
    offline_hits: u8,
    giving_way: bool,
    idle_known: bool,
    last_dto: Option<QueuesDto>,
}

fn fmt_time(t: NaiveDateTime) -> String {
    t.format(TIME_FMT).to_string()
}

/// `YYYY-MM-DDTHH:MM` (what the UI sends and the database keeps).
pub(super) fn parse_local(s: &str) -> Option<NaiveDateTime> {
    NaiveDateTime::parse_from_str(s.trim(), TIME_FMT).ok()
}

fn net_kind(k: ConnectionKind) -> NetKind {
    match k {
        ConnectionKind::Wifi => NetKind::Wifi,
        ConnectionKind::Wired => NetKind::Wired,
        ConnectionKind::Mobile => NetKind::Mobile,
        ConnectionKind::Other => NetKind::Other,
    }
}

fn gate_dto(g: Option<Closed>) -> (QueueGate, Option<NaiveDateTime>) {
    match g {
        None => (QueueGate::Open, None),
        Some(Closed::Paused) => (QueueGate::Paused, None),
        Some(Closed::Time { opens_at }) => (QueueGate::Time, opens_at),
        Some(Closed::Offline) => (QueueGate::Offline, None),
        Some(Closed::Connection) => (QueueGate::Connection, None),
        Some(Closed::NotIdle) => (QueueGate::NotIdle, None),
        Some(Closed::Busy) => (QueueGate::Busy, None),
    }
}

/// Still has to download: queued (waiting, or added for later) or running. Paused and failed ones wait for the user.
fn pending(status: DownloadStatus) -> bool {
    matches!(status, DownloadStatus::Queued | DownloadStatus::Downloading | DownloadStatus::Verifying)
}

impl Queues {
    pub(super) fn load(store: &SqliteStore) -> Self {
        let get = |k: &str| store.get_setting(k).ok().flatten();
        let mut defs: Vec<QueueDef> = get(DEFS_KEY)
            .and_then(|s| serde_json::from_str::<Vec<QueueDef>>(&s).ok())
            .unwrap_or_default()
            .into_iter()
            .filter(|d| d.rules.validate().is_ok() && d.id >= MAIN_QUEUE)
            .take(MAX_QUEUES)
            .collect();
        let mut seen = HashSet::new();
        defs.retain(|d| seen.insert(d.id));
        if !defs.iter().any(|d| d.id == MAIN_QUEUE) {
            defs.insert(0, QueueDef { id: MAIN_QUEUE, name: String::new(), rules: QueueRules::default() });
        }
        let saved: HashMap<i64, Saved> = get(STATE_KEY).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        let rt = defs
            .iter()
            .map(|d| {
                let s = saved.get(&d.id).cloned().unwrap_or_default();
                let rule = RuleState::restored(s.used_until.as_deref().and_then(parse_local));
                (d.id, Runtime { paused: s.paused, forced: s.forced, rule, ..Default::default() })
            })
            .collect();
        let places = store.placements().unwrap_or_default().into_iter().map(|p| (p.download_id, p)).collect();
        let held = get(HELD_KEY).map(|s| s.split(',').filter_map(|p| p.parse::<i64>().ok()).collect()).unwrap_or_default();
        Self {
            defs,
            rt,
            places,
            held,
            forced: HashSet::new(),
            net: NetworkState::default(),
            net_at: None,
            offline_hits: 0,
            giving_way: false,
            idle_known: false,
            last_dto: None,
        }
    }

    /// Where a download sits (a default place for one that was never moved).
    pub(super) fn place(&self, id: i64) -> Placement {
        self.places.get(&id).cloned().unwrap_or(Placement {
            download_id: id,
            queue_id: MAIN_QUEUE,
            position: id.saturating_mul(GAP),
            start_at: None,
            pinned: false,
        })
    }

    fn queue_of(&self, id: i64) -> i64 {
        let q = self.place(id).queue_id;
        if self.rt.contains_key(&q) {
            q
        } else {
            MAIN_QUEUE
        }
    }

    fn index_of(&self, queue: i64) -> usize {
        self.defs.iter().position(|d| d.id == queue).unwrap_or(0)
    }

    /// The order the slot scheduler uses after priority: the queue order, then the place in the queue.
    fn rank(&self, id: i64) -> i64 {
        ((self.index_of(self.queue_of(id)) as i64) << 44) + self.place(id).position.clamp(-(1 << 43), 1 << 43)
    }

    fn saved_state(&self) -> String {
        let m: HashMap<i64, Saved> = self
            .rt
            .iter()
            .map(|(id, r)| (*id, Saved { paused: r.paused, forced: r.forced, used_until: r.rule.used_until.map(fmt_time) }))
            .collect();
        serde_json::to_string(&m).unwrap_or_default()
    }

    fn dto(&self, now: NaiveDateTime) -> QueuesDto {
        let until = now + Span::hours(HORIZON_HOURS);
        QueuesDto {
            queues: self
                .defs
                .iter()
                .map(|d| {
                    let rt = self.rt.get(&d.id);
                    let (gate, opens_at) = gate_dto(rt.and_then(|r| r.gate));
                    QueueDto {
                        id: d.id,
                        name: d.name.clone(),
                        rules: d.rules.clone(),
                        paused: rt.is_some_and(|r| r.paused),
                        gate,
                        opens_at: opens_at.map(fmt_time),
                        forced: rt.is_some_and(|r| r.forced),
                        spans: d
                            .rules
                            .spans(now, until)
                            .into_iter()
                            .map(|(a, b)| QueueSpanDto { from: fmt_time(a), to: fmt_time(b) })
                            .collect(),
                    }
                })
                .collect(),
            giving_way: self.giving_way,
            idle_known: self.idle_known,
            online: self.net.online,
            connection: self.net.kind,
            now: fmt_time(now),
        }
    }
}

/// What one evaluation decided to do (done after the locks are released).
#[derive(Default)]
struct Plan {
    hold: Vec<i64>,
    release: Vec<i64>,
    due: Vec<i64>,
    auto_start: Vec<i64>,
    routed: Vec<(i64, bool)>,
    save_state: bool,
}

impl DownloadManager {
    // ------------------------------------------------------------------ public API

    pub async fn get_queues(&self) -> Result<QueuesDto, ManagerError> {
        Ok(self.inner.queues.lock().unwrap().dto(self.now_local()))
    }

    /// Create (`id: None`) or change a queue: its name and its rules.
    pub async fn save_queue(&self, req: SaveQueue) -> Result<QueuesDto, ManagerError> {
        let name: String = req.name.trim().chars().filter(|c| !c.is_control()).collect();
        if name.chars().count() > MAX_NAME {
            return Err(ManagerError::invalid(format!("a queue name has at most {MAX_NAME} characters")));
        }
        req.rules.validate().map_err(|e| ManagerError::invalid(e.to_string()))?;
        let now = self.now_local();
        {
            let mut q = self.inner.queues.lock().unwrap();
            match req.id {
                Some(id) => {
                    let def = q.defs.iter_mut().find(|d| d.id == id).ok_or_else(|| ManagerError::invalid("this queue no longer exists"))?;
                    if id != MAIN_QUEUE && name.is_empty() {
                        return Err(ManagerError::invalid("give the queue a name"));
                    }
                    let rules_changed = def.rules != req.rules;
                    def.name = if id == MAIN_QUEUE { String::new() } else { name };
                    def.rules = req.rules.clone();
                    if rules_changed {
                        q.net_at = None; // a connection rule applies at once
                                         // A start time that already passed today waits for tomorrow; counters start over.
                        let rt = q.rt.entry(id).or_default();
                        rt.rule = req.rules.fresh_state(now);
                        rt.forced = false;
                    }
                }
                None => {
                    if name.is_empty() {
                        return Err(ManagerError::invalid("give the queue a name"));
                    }
                    if q.defs.len() >= MAX_QUEUES {
                        return Err(ManagerError::invalid(format!("at most {MAX_QUEUES} queues")));
                    }
                    let id = q.defs.iter().map(|d| d.id).max().unwrap_or(MAIN_QUEUE) + 1;
                    q.net_at = None;
                    q.defs.push(QueueDef { id, name, rules: req.rules.clone() });
                    q.rt.insert(id, Runtime { rule: req.rules.fresh_state(now), ..Default::default() });
                }
            }
        }
        self.save_queue_defs().await?;
        self.evaluate_queues().await;
        self.get_queues().await
    }

    /// Delete a queue (not the main one): its downloads go to the main queue.
    pub async fn delete_queue(&self, id: i64) -> Result<QueuesDto, ManagerError> {
        if id == MAIN_QUEUE {
            return Err(ManagerError::invalid("the main queue cannot be deleted"));
        }
        let (moved, released) = {
            let mut q = self.inner.queues.lock().unwrap();
            if !q.defs.iter().any(|d| d.id == id) {
                return Err(ManagerError::invalid("this queue no longer exists"));
            }
            q.defs.retain(|d| d.id != id);
            q.rt.remove(&id);
            let mut moved = Vec::new();
            for p in q.places.values_mut().filter(|p| p.queue_id == id) {
                p.queue_id = MAIN_QUEUE;
                p.pinned = false;
                moved.push(p.download_id);
            }
            // What the deleted queue had paused is resumed: its rules are gone.
            let released: Vec<i64> = q.held.iter().copied().filter(|h| moved.contains(h)).collect();
            q.held.retain(|h| !released.contains(h));
            (moved, released)
        };
        let store = self.inner.store.clone();
        blocking(move || store.move_queue(id, MAIN_QUEUE)).await?;
        self.save_queue_defs().await?;
        self.save_queue_held().await;
        self.resume_ids(&released, false).await;
        for d in moved {
            self.emit_download(d).await;
        }
        self.evaluate_queues().await;
        self.get_queues().await
    }

    /// Start, pause, clear, reorder or re-prioritise a whole queue.
    pub async fn queue_action(&self, id: i64, action: QueueAction) -> Result<QueuesDto, ManagerError> {
        if !self.inner.queues.lock().unwrap().rt.contains_key(&id) {
            return Err(ManagerError::invalid("this queue no longer exists"));
        }
        match action {
            QueueAction::Start => {
                let held = {
                    let mut q = self.inner.queues.lock().unwrap();
                    let rt = q.rt.get_mut(&id).expect("checked");
                    rt.paused = false;
                    rt.forced = true;
                    let held: Vec<i64> = q.held.iter().copied().filter(|h| q.queue_of(*h) == id).collect();
                    q.held.retain(|h| !held.contains(h));
                    held
                };
                self.save_queue_defs().await?;
                self.save_queue_held().await;
                // Everything in it that is not finished and not running: paused, failed, or added for later.
                let rows = self.queue_rows().await;
                let mut todo: Vec<(i64, i64)> = {
                    let q = self.inner.queues.lock().unwrap();
                    rows.iter()
                        .filter(|r| q.queue_of(r.id) == id && matches!(r.status, DownloadStatus::Queued | DownloadStatus::Paused))
                        .filter(|r| !self.is_active(r.id))
                        .map(|r| (q.rank(r.id) - r.priority.as_i64() * (1 << 50), r.id))
                        .collect()
                };
                todo.sort();
                self.resume_ids(&held, false).await;
                for (_, d) in todo {
                    self.clear_start_at(d).await;
                    let _ = self.start_inner(d).await;
                }
                self.inner.queue.lock().unwrap().note_user_start();
            }
            QueueAction::Pause => {
                {
                    let mut q = self.inner.queues.lock().unwrap();
                    let rt = q.rt.get_mut(&id).expect("checked");
                    rt.paused = true;
                    rt.forced = false;
                }
                self.save_queue_defs().await?;
            }
            QueueAction::ClearCompleted => {
                let store = self.inner.store.clone();
                let done: Vec<i64> = blocking(move || store.list())
                    .await?
                    .into_iter()
                    .filter(|r| r.status == DownloadStatus::Completed)
                    .filter_map(|r| r.id)
                    .collect();
                let mine: Vec<i64> = {
                    let q = self.inner.queues.lock().unwrap();
                    done.into_iter().filter(|d| q.queue_of(*d) == id).collect()
                };
                for d in mine {
                    let _ = self.delete_download(d, false).await;
                }
            }
            QueueAction::MoveUp | QueueAction::MoveDown => {
                {
                    let mut q = self.inner.queues.lock().unwrap();
                    let i = q.index_of(id);
                    let j = if action == QueueAction::MoveUp { i.checked_sub(1) } else { Some(i + 1).filter(|j| *j < q.defs.len()) };
                    if let Some(j) = j {
                        q.defs.swap(i, j);
                    }
                }
                self.save_queue_defs().await?;
            }
            QueueAction::Priority { priority } => {
                let rows = self.queue_rows().await;
                let mine: Vec<i64> = {
                    let q = self.inner.queues.lock().unwrap();
                    rows.iter().filter(|r| q.queue_of(r.id) == id && r.priority != priority).map(|r| r.id).collect()
                };
                for d in mine {
                    let _ = self.set_priority(d, priority).await;
                }
            }
        }
        self.evaluate_queues().await;
        self.get_queues().await
    }

    /// Move a download to another queue, set when it starts, or move it up or down in its queue.
    pub async fn place_download(&self, req: PlaceDownload) -> Result<DownloadDto, ManagerError> {
        let rec = self.load(req.id).await?;
        let id = req.id;
        let mut changed: Vec<i64> = Vec::new();
        let mut resume = false;
        if let Some(queue) = req.queue_id {
            let mut q = self.inner.queues.lock().unwrap();
            if !q.rt.contains_key(&queue) {
                return Err(ManagerError::invalid("this queue no longer exists"));
            }
            let mut p = q.place(id);
            p.queue_id = queue;
            p.pinned = true;
            q.places.insert(id, p);
            // Paused by its old queue's rules: it goes on, and the new queue's rules decide when it runs.
            if q.held.contains(&id) {
                q.held.retain(|h| *h != id);
                resume = true;
            }
            changed.push(id);
        }
        if let Some(step) = req.step {
            if matches!(rec.status, DownloadStatus::Completed | DownloadStatus::Cancelled) {
                return Err(ManagerError::InvalidState { action: "move", status: rec.status });
            }
            let rows = self.queue_rows().await;
            let mut q = self.inner.queues.lock().unwrap();
            let queue = q.queue_of(id);
            // The neighbours it can trade places with: same queue, same priority, not finished.
            let mut line: Vec<(i64, i64)> = rows
                .iter()
                .filter(|r| q.queue_of(r.id) == queue && r.priority == rec.priority)
                .map(|r| (q.place(r.id).position, r.id))
                .collect();
            line.sort();
            if let Some(i) = line.iter().position(|(_, d)| *d == id) {
                let other = match step {
                    QueueStep::Up => i.checked_sub(1),
                    QueueStep::Down => Some(i + 1).filter(|j| *j < line.len()),
                    QueueStep::Top => None,
                };
                if let Some(j) = other {
                    let (a, b) = (line[i], line[j]);
                    let (mut pa, mut pb) = (q.place(a.1), q.place(b.1));
                    // Equal positions cannot swap: give them distinct ones first.
                    let (x, y) = if a.0 == b.0 { (a.0, a.0 + if j > i { 1 } else { -1 }) } else { (a.0, b.0) };
                    pa.position = y;
                    pb.position = x;
                    q.places.insert(a.1, pa);
                    q.places.insert(b.1, pb);
                    changed.extend([a.1, b.1]);
                } else if step == QueueStep::Top && i > 0 {
                    let mut p = q.place(id);
                    p.position = line[0].0 - GAP;
                    q.places.insert(id, p);
                    changed.push(id);
                }
            }
        }
        let neighbours: Vec<i64> = changed.iter().copied().filter(|d| *d != id).collect();
        let changed: Vec<Placement> = {
            let q = self.inner.queues.lock().unwrap();
            changed.iter().map(|d| q.place(*d)).collect()
        };
        self.save_places(changed).await;
        // The download it traded places with moved too: the UI orders by both.
        for d in neighbours {
            self.emit_download(d).await;
        }
        if resume {
            self.save_queue_held().await;
            self.resume_ids(&[id], true).await;
        }
        if let Some(when) = req.start {
            match when {
                StartWhen::Now => {
                    self.clear_start_at(id).await;
                    self.inner.queues.lock().unwrap().forced.insert(id);
                    let held = {
                        let mut q = self.inner.queues.lock().unwrap();
                        let was = q.held.contains(&id);
                        q.held.retain(|h| *h != id);
                        was
                    };
                    if held {
                        self.save_queue_held().await;
                    }
                    if !self.is_active(id) {
                        self.start_download(id).await?;
                    }
                }
                StartWhen::Later => {
                    self.clear_start_at(id).await;
                    self.inner.queues.lock().unwrap().forced.remove(&id);
                    if self.is_active(id) {
                        self.pause_download(id).await?;
                    }
                }
                StartWhen::At { at } => {
                    let t = parse_local(&at).ok_or_else(|| ManagerError::invalid("the start time must look like 2026-03-02T02:00"))?;
                    if t > self.now_local() + Span::days(366) {
                        return Err(ManagerError::invalid("the start time is more than a year away"));
                    }
                    if matches!(rec.status, DownloadStatus::Completed | DownloadStatus::Cancelled) {
                        return Err(ManagerError::InvalidState { action: "schedule", status: rec.status });
                    }
                    self.inner.queues.lock().unwrap().forced.remove(&id);
                    if self.is_active(id) {
                        self.pause_download(id).await?;
                    }
                    let p = {
                        let mut q = self.inner.queues.lock().unwrap();
                        let mut p = q.place(id);
                        p.start_at = Some(fmt_time(t));
                        q.places.insert(id, p.clone());
                        p
                    };
                    self.save_places(vec![p]).await;
                }
            }
        }
        self.evaluate_queues().await;
        let dto = self.dto_for(id).await?;
        self.emit(ManagerEvent::Updated { download: dto.clone() });
        Ok(dto)
    }

    // ------------------------------------------------------------------ for the rest of the manager

    /// A new download: put it in the queue it was added to, with its start time.
    pub(super) async fn place_new(&self, id: i64, queue: Option<i64>, start_at: Option<NaiveDateTime>) {
        if queue.is_none() && start_at.is_none() {
            return;
        }
        let p = {
            let mut q = self.inner.queues.lock().unwrap();
            let mut p = q.place(id);
            if let Some(queue) = queue.filter(|q2| q.rt.contains_key(q2)) {
                p.queue_id = queue;
                p.pinned = true;
            }
            p.start_at = start_at.map(fmt_time);
            q.places.insert(id, p.clone());
            p
        };
        self.save_places(vec![p]).await;
    }

    /// Check what a new download asks of the queue before it is created.
    pub(super) fn check_new_placement(&self, req: &NewDownload) -> Result<Option<NaiveDateTime>, ManagerError> {
        if let Some(q) = req.queue_id {
            if !self.inner.queues.lock().unwrap().rt.contains_key(&q) {
                return Err(ManagerError::invalid("this queue no longer exists"));
            }
        }
        let Some(at) = req.start_at.as_deref() else { return Ok(None) };
        let t = parse_local(at).ok_or_else(|| ManagerError::invalid("the start time must look like 2026-03-02T02:00"))?;
        if t > self.now_local() + Span::days(366) {
            return Err(ManagerError::invalid("the start time is more than a year away"));
        }
        Ok(Some(t))
    }

    /// A download is about to ask for a run slot: give the scheduler its order and hold it back right away if its
    /// queue is closed (the next evaluation would, but a free slot could be taken first).
    pub(super) fn policy_for_new_run(&self, id: i64, priority: Priority) {
        let giving_way = self.smart_is_limiting();
        let (rank, blocked) = {
            let q = self.inner.queues.lock().unwrap();
            let closed = q.rt.get(&q.queue_of(id)).is_some_and(|rt| !rt.open());
            let blocked = !q.forced.contains(&id) && (closed || (giving_way && priority <= Priority::Low));
            (q.rank(id), blocked)
        };
        self.inner.scheduler.set_one(id as u64, rank, blocked);
    }

    /// A download was deleted (its saved place went with it).
    pub(super) fn forget_placement(&self, id: i64) {
        let mut q = self.inner.queues.lock().unwrap();
        q.places.remove(&id);
        q.forced.remove(&id);
        q.held.retain(|h| *h != id);
    }

    /// The user started or paused it by hand: the queue no longer resumes it on its own.
    pub(super) async fn user_took_over(&self, id: i64) {
        let was = {
            let mut q = self.inner.queues.lock().unwrap();
            let was = q.held.contains(&id);
            q.held.retain(|h| *h != id);
            was
        };
        if was {
            self.save_queue_held().await;
        }
    }

    /// Whether the computer is online, as the queues judge it: a download making progress proves it; "offline"
    /// counts only once seen twice in a row; `None` when not known. Second: the last look said offline (not
    /// confirmed yet).
    pub(super) fn judged_online(&self) -> (Option<bool>, bool) {
        if self.inner.trackers.lock().unwrap().values().any(|t| t.speed > 1024.0) {
            return (Some(true), false);
        }
        let q = self.inner.queues.lock().unwrap();
        let seen_offline = q.net.online == Some(false);
        if seen_offline && q.offline_hits < OFFLINE_CONFIRM {
            (None, true)
        } else {
            (q.net.online, seen_offline)
        }
    }

    /// Paused by its queue's rules (resumed by the queue).
    pub(super) fn held_by_queue(&self, id: i64) -> bool {
        self.inner.queues.lock().unwrap().held.contains(&id)
    }

    pub(super) fn placement_of(&self, id: i64) -> Placement {
        self.inner.queues.lock().unwrap().place(id)
    }

    // ------------------------------------------------------------------ evaluation

    fn now_local(&self) -> NaiveDateTime {
        (self.inner.cfg.clock.0)()
    }

    async fn queue_rows(&self) -> Vec<QueueRow> {
        let store = self.inner.store.clone();
        blocking(move || store.queue_rows()).await.unwrap_or_default()
    }

    /// Judge every queue's rules and act on what changed. Runs every second, and after each change.
    pub(super) async fn evaluate_queues(&self) {
        if self.inner.shutting_down.load(Ordering::Relaxed) {
            return;
        }
        let _serial = self.inner.queue_lock.lock().await;
        let now = self.now_local();
        let rows = self.queue_rows().await;
        let (needs_idle, needs_traffic, has_rules) = {
            let q = self.inner.queues.lock().unwrap();
            (
                q.defs.iter().any(|d| d.rules.uses_idle()),
                q.defs.iter().any(|d| d.rules.uses_traffic()),
                q.defs.iter().any(|d| !d.rules.is_plain()),
            )
        };
        let system = self.inner.cfg.system.0.clone();
        let idle = if needs_idle { tokio::task::spawn_blocking(move || system.user_idle()).await.ok().flatten() } else { None };
        // The network: only looked at when something has to download (or a rule depends on it), every few seconds.
        let work = rows.iter().any(|r| pending(r.status));
        let every = self.inner.cfg.queue_poll * NET_EVERY_POLLS;
        let net_due =
            (work || has_rules || self.recovering()) && self.inner.queues.lock().unwrap().net_at.is_none_or(|t| t.elapsed() >= every);
        if net_due {
            let system = self.inner.cfg.system.0.clone();
            let net = tokio::task::spawn_blocking(move || system.network()).await.unwrap_or_default();
            let mut q = self.inner.queues.lock().unwrap();
            q.offline_hits = if net.online == Some(false) { q.offline_hits.saturating_add(1) } else { 0 };
            q.net = net;
            q.net_at = Some(Instant::now());
        }
        // Traffic that is not a queue's own: everything measured minus what that queue downloads.
        let net_down = if needs_traffic {
            let root = self.inner.storage.lock().unwrap_or_else(|p| p.into_inner()).root.clone();
            self.inner.sysio.refresh(root);
            self.inner.sysio.rates().net_down
        } else {
            None
        };
        let speeds: Vec<(i64, f64)> = self.inner.trackers.lock().unwrap().iter().map(|(id, t)| (*id, t.speed)).collect();
        let giving_way = self.smart_is_limiting();
        // A download that makes progress proves the network works, whatever the routing table says.
        let progressing = speeds.iter().any(|(_, s)| *s > 1024.0);
        // Active downloads, and whether each holds a run slot (not waiting for one, nor about to ask).
        let running: Vec<(i64, bool)> = {
            let active = self.inner.active.lock().unwrap();
            active.iter().map(|(id, a)| (*id, a.progress.queue_key().is_some_and(|k| !self.inner.scheduler.is_waiting(k)))).collect()
        };
        let active_ids: HashSet<i64> = running.iter().map(|(id, _)| *id).collect();

        let mut plan = Plan::default();
        let (ranks, blocked, dto) = {
            let mut q = self.inner.queues.lock().unwrap();
            q.giving_way = giving_way;
            q.idle_known = !needs_idle || idle.is_some();
            let online = if progressing {
                Some(true)
            } else if q.net.online == Some(false) && q.offline_hits < OFFLINE_CONFIRM {
                None
            } else {
                q.net.online
            };
            let net = q.net.kind.map(net_kind);
            // The pruning of "start now" marks: once a download is no longer active, the mark is spent.
            q.forced.retain(|id| active_ids.contains(id));
            let defs = q.defs.clone();
            for d in &defs {
                let own: f64 = speeds.iter().filter(|(id, _)| q.queue_of(*id) == d.id).map(|(_, s)| s).sum();
                let has_work =
                    rows.iter().any(|r| pending(r.status) && q.queue_of(r.id) == d.id) || q.held.iter().any(|h| q.queue_of(*h) == d.id);
                let rt = q.rt.entry(d.id).or_default();
                let other = net_down.map(|n| (n as f64 - own).max(0.0));
                rt.other_bps = match (rt.other_bps, other) {
                    (Some(prev), Some(o)) => Some(prev + TRAFFIC_EMA * (o - prev)),
                    (_, o) => o,
                };
                let inputs = Inputs { now, idle, net, online, other_bps: rt.other_bps.map(|v| v as u64) };
                let before = rt.rule.used_until;
                let was_open = rt.applied_open == Some(true);
                let judged = d.rules.evaluate(&mut rt.rule, &inputs, has_work, was_open).err();
                if rt.rule.used_until != before {
                    plan.save_state = true;
                }
                if rt.forced && !has_work {
                    rt.forced = false; // "Start queue" lasts until the queue is empty
                    plan.save_state = true;
                }
                rt.gate = if rt.paused {
                    Some(Closed::Paused)
                } else if rt.forced {
                    // Started by hand: only a missing network still stops new starts.
                    judged.filter(|c| *c == Closed::Offline)
                } else {
                    judged
                };
                let open = rt.open();
                let prev = rt.applied_open;
                rt.applied_open = Some(open);
                match (open, prev) {
                    (false, p) if p != Some(false) && rt.gate.is_some_and(Closed::pauses_running) => {
                        // Running downloads of the queue pause (not those started by hand).
                        for (id, has_slot) in &running {
                            if *has_slot && q.queue_of(*id) == d.id && !q.forced.contains(id) {
                                plan.hold.push(*id);
                            }
                        }
                    }
                    (true, p) if p != Some(true) => {
                        plan.release.extend(q.held.iter().copied().filter(|h| q.queue_of(*h) == d.id));
                    }
                    _ => {}
                }
            }
            q.held.retain(|h| !plan.release.contains(h));
            // Start times that came, and what a queue with rules has to start now that it is open.
            for r in &rows {
                if active_ids.contains(&r.id) || !matches!(r.status, DownloadStatus::Queued | DownloadStatus::Paused) {
                    continue;
                }
                let p = q.place(r.id);
                match p.start_at.as_deref().and_then(parse_local) {
                    Some(t) if t <= now => plan.due.push(r.id),
                    Some(_) => {}
                    None => {
                        let queue = q.queue_of(r.id);
                        let auto = q.defs.iter().any(|d| d.id == queue && !d.rules.is_plain());
                        if auto && r.status == DownloadStatus::Queued && q.rt.get(&queue).is_some_and(Runtime::open) {
                            plan.auto_start.push(r.id);
                        }
                    }
                }
            }
            // Big files go to the queue made for them (once, while little is done, never against the user's choice).
            let big: Vec<(i64, u64)> = q.defs.iter().filter_map(|d| d.rules.large_files_over.map(|s| (d.id, s))).collect();
            if !big.is_empty() {
                for r in &rows {
                    let Some(total) = r.total_size.filter(|t| *t > 0) else { continue };
                    let p = q.place(r.id);
                    if p.pinned || q.forced.contains(&r.id) || r.downloaded > total / ROUTE_BEFORE_SHARE || !pending(r.status) {
                        continue;
                    }
                    let current = q.queue_of(r.id);
                    let fits_here = big.iter().any(|(id, s)| *id == current && total > *s);
                    let Some((to, _)) = big.iter().find(|(id, s)| *id != current && total > *s).copied() else { continue };
                    if fits_here {
                        continue;
                    }
                    let mut p = p;
                    p.queue_id = to;
                    q.places.insert(r.id, p);
                    let closed = q.rt.get(&to).is_some_and(|rt| rt.gate.is_some_and(Closed::pauses_running));
                    let is_running = running.iter().any(|(id, has_slot)| *id == r.id && *has_slot);
                    if closed && is_running {
                        if r.resumable {
                            plan.hold.push(r.id);
                        } else {
                            // Pausing would lose what it has: it finishes where it is.
                            let mut back = q.place(r.id);
                            back.queue_id = current;
                            back.pinned = true;
                            q.places.insert(r.id, back);
                            continue;
                        }
                    }
                    plan.routed.push((r.id, closed));
                }
            }
            // The scheduler's order and the downloads it must hold back.
            let prio: HashMap<i64, Priority> = rows.iter().map(|r| (r.id, r.priority)).collect();
            let mut ranks = HashMap::new();
            let mut blocked = HashSet::new();
            // Every active download counts, also one not yet asking for a slot (holding back one that already
            // has a slot changes nothing).
            for (id, _) in &running {
                let key = *id as u64;
                ranks.insert(key, q.rank(*id));
                if q.forced.contains(id) {
                    continue;
                }
                let queue = q.queue_of(*id);
                let closed = q.rt.get(&queue).is_some_and(|rt| !rt.open());
                let low = prio.get(id).is_some_and(|p| *p <= Priority::Low);
                if closed || (giving_way && low) {
                    blocked.insert(key);
                }
            }
            (ranks, blocked, q.dto(now))
        };
        self.inner.scheduler.set_policy(ranks, blocked);
        drop(_serial);

        // ---- act (no lock held: pausing waits for downloads to save)
        if plan.save_state {
            let _ = self.save_queue_defs().await;
        }
        if !plan.hold.is_empty() {
            self.hold_for_queue(&plan.hold).await;
        }
        if !plan.release.is_empty() {
            self.save_queue_held().await;
            self.resume_ids(&plan.release, true).await;
        }
        for id in plan.due {
            self.clear_start_at(id).await;
            self.inner.queues.lock().unwrap().forced.insert(id);
            if self.start_inner(id).await.is_err() {
                self.inner.queues.lock().unwrap().forced.remove(&id);
            }
        }
        for id in plan.auto_start {
            let _ = self.start_inner(id).await;
        }
        if !plan.routed.is_empty() {
            let places: Vec<Placement> = {
                let q = self.inner.queues.lock().unwrap();
                plan.routed.iter().map(|(id, _)| q.place(*id)).collect()
            };
            self.save_places(places).await;
            for (id, _) in plan.routed {
                tracing::info!(id, "a big download moved to the queue made for big files");
                self.emit_download(id).await;
            }
        }
        let changed = {
            let mut q = self.inner.queues.lock().unwrap();
            let changed = q.last_dto.as_ref() != Some(&dto);
            q.last_dto = Some(dto.clone());
            changed
        };
        if changed {
            let _ = self.inner.events.send(ManagerEvent::Queues { queues: dto });
        }
    }

    /// Pause these running downloads for their queue and remember them.
    async fn hold_for_queue(&self, ids: &[i64]) {
        let targets: Vec<(i64, DownloadControl, watch::Receiver<bool>)> = {
            let active = self.inner.active.lock().unwrap();
            ids.iter().filter_map(|id| active.get(id).map(|a| (*id, a.control.clone(), a.done.clone()))).collect()
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
        {
            let mut q = self.inner.queues.lock().unwrap();
            for id in &paused {
                if !q.held.contains(id) {
                    q.held.push(*id);
                }
            }
        }
        self.save_queue_held().await;
        for id in paused {
            self.emit_download(id).await; // now shown as paused by its queue
        }
    }

    /// Start these again if they are still paused (the user may have deleted or started one meanwhile).
    async fn resume_ids(&self, ids: &[i64], preferred: bool) {
        for &id in ids {
            if matches!(self.load(id).await, Ok(rec) if rec.status == DownloadStatus::Paused) {
                if preferred {
                    self.inner.scheduler.prefer(id as u64); // they ran before: ahead of those that never started
                }
                let _ = self.start_inner(id).await;
            }
        }
    }

    async fn clear_start_at(&self, id: i64) {
        let p = {
            let mut q = self.inner.queues.lock().unwrap();
            match q.places.get_mut(&id) {
                Some(p) if p.start_at.is_some() => {
                    p.start_at = None;
                    p.clone()
                }
                _ => return,
            }
        };
        self.save_places(vec![p]).await;
    }

    async fn save_places(&self, places: Vec<Placement>) {
        if places.is_empty() {
            return;
        }
        let store = self.inner.store.clone();
        let res = blocking(move || places.iter().try_for_each(|p| store.set_placement(p))).await;
        if let Err(e) = res {
            tracing::warn!(error = %e, "could not save the place of a download in its queue");
        }
    }

    async fn save_queue_defs(&self) -> Result<(), ManagerError> {
        let (defs, state) = {
            let q = self.inner.queues.lock().unwrap();
            (serde_json::to_string(&q.defs).map_err(|e| ManagerError::Internal(e.to_string()))?, q.saved_state())
        };
        let store = self.inner.store.clone();
        blocking(move || {
            store.set_setting(DEFS_KEY, &defs)?;
            store.set_setting(STATE_KEY, &state)
        })
        .await?;
        Ok(())
    }

    async fn save_queue_held(&self) {
        let csv = self.inner.queues.lock().unwrap().held.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
        let store = self.inner.store.clone();
        let _ = blocking(move || if csv.is_empty() { store.delete_setting(HELD_KEY) } else { store.set_setting(HELD_KEY, &csv) }).await;
    }

    async fn emit_download(&self, id: i64) {
        if let Ok(dto) = self.dto_for(id).await {
            self.emit(ManagerEvent::Updated { download: dto });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_times_round_trip_and_bad_ones_are_refused() {
        let t = parse_local("2026-03-02T02:05").unwrap();
        assert_eq!(fmt_time(t), "2026-03-02T02:05");
        for bad in ["", "02:00", "2026-03-02 02:00", "2026-13-02T02:00", "2026-03-02T25:00", "2026-03-02T02:00:00"] {
            assert!(parse_local(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn downloads_without_a_place_sit_in_the_main_queue_in_the_order_they_were_added() {
        let store = SqliteStore::open_in_memory().unwrap();
        let q = Queues::load(&store);
        assert_eq!(q.defs.len(), 1);
        assert_eq!((q.defs[0].id, q.defs[0].name.as_str()), (MAIN_QUEUE, ""));
        assert!(q.defs[0].rules.is_plain(), "the main queue has no rule: the old behaviour");
        assert_eq!(q.place(7).queue_id, MAIN_QUEUE);
        assert!(q.rank(3) < q.rank(4));
        assert_eq!(gate_dto(None).0, QueueGate::Open);
    }

    #[test]
    fn saved_queues_that_do_not_make_sense_are_ignored() {
        let store = SqliteStore::open_in_memory().unwrap();
        store.set_setting(DEFS_KEY, "not json").unwrap();
        assert_eq!(Queues::load(&store).defs.len(), 1);
        let bad = r#"[{"id":2,"name":"Night","rules":{"when":{"kind":"between","start":"23:00","stop":"23:00","days":[true,true,true,true,true,true,true]}}},
                      {"id":3,"name":"Ok","rules":{}},{"id":3,"name":"Twice","rules":{}}]"#;
        store.set_setting(DEFS_KEY, bad).unwrap();
        let q = Queues::load(&store);
        assert_eq!(
            q.defs.iter().map(|d| d.id).collect::<Vec<_>>(),
            vec![MAIN_QUEUE, 3],
            "invalid rules and duplicates dropped, main added"
        );
    }
}
