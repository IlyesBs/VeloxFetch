//! Phase 6: queue control (max active, connections, pause/resume queue), the time-window
//! scheduler, and the opt-in power action. Includes regression guards: with nothing configured the
//! manager must behave exactly as it did before this phase.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use download_manager::{
    Clock, DownloadManager, ManagerConfig, ManagerError, ManagerEvent, NewDownload, PauseQueue, PowerAction, PowerPhase, QueueStateDto,
    Schedule, SetPowerAction, SetQueueConfig, SystemHandle,
};
use downloader_core::{AdaptiveConfig, DownloadStatus, RetryPolicy};
use scheduler::chrono::{NaiveDate, NaiveDateTime};
use system_integration::DryRunSystem;
use velox_integration_tests::{random_data, serve, Server};

const MB: usize = 1024 * 1024;

struct Env {
    db: PathBuf,
    root: PathBuf,
}

fn env(name: &str) -> Env {
    let base = std::env::temp_dir().join(format!("velox-p6-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    Env { db: base.join("state").join("velox.db"), root: base.join("downloads") }
}

fn at(d: u32, h: u32, m: u32) -> NaiveDateTime {
    // 2026-03-02 is a Monday.
    NaiveDate::from_ymd_opt(2026, 3, d).unwrap().and_hms_opt(h, m, 0).unwrap()
}

#[derive(Clone)]
struct FakeClock(Arc<Mutex<NaiveDateTime>>);

impl FakeClock {
    fn new(t: NaiveDateTime) -> Self {
        Self(Arc::new(Mutex::new(t)))
    }
    fn set(&self, t: NaiveDateTime) {
        *self.0.lock().unwrap() = t;
    }
    fn clock(&self) -> Clock {
        let c = self.0.clone();
        Clock(Arc::new(move || *c.lock().unwrap()))
    }
}

/// Records what would have been run. Nothing in this test file can touch the real machine.
/// The pretend system, with control over which power actions exist and whether running one fails.
fn fake_power() -> Arc<DryRunSystem> {
    Arc::new(DryRunSystem::new())
}

struct Opts {
    slots: usize,
    clock: Option<FakeClock>,
    power: Option<Arc<DryRunSystem>>,
    grace: Duration,
    adaptive: bool,
}

impl Default for Opts {
    fn default() -> Self {
        Self { slots: 3, clock: None, power: None, grace: Duration::from_millis(300), adaptive: true }
    }
}

async fn manager_at(e: &Env, o: Opts) -> DownloadManager {
    let mut c = ManagerConfig::new(e.db.clone(), e.root.clone());
    c.max_concurrent_downloads = o.slots;
    c.queue_poll = Duration::from_millis(30);
    c.power_grace = o.grace;
    c.download.min_segment_size = 64 * 1024;
    c.download.checkpoint_interval = Duration::from_millis(50);
    c.download.retry = RetryPolicy { max_retries: 1, max_throttle_retries: 1, ..Default::default() };
    c.download.adaptive = AdaptiveConfig { enabled: o.adaptive, interval: Duration::from_millis(150), ..Default::default() };
    if let Some(clock) = o.clock {
        c.clock = clock.clock();
    }
    if let Some(p) = o.power {
        c.system = SystemHandle(p);
    }
    DownloadManager::new(c).await.unwrap()
}

async fn manager(name: &str, slots: usize) -> (DownloadManager, Env) {
    let e = env(name);
    let m = manager_at(&e, Opts { slots, ..Default::default() }).await;
    (m, e)
}

/// A download throttled to `bps` so it stays "running" for a predictable time.
fn slow(srv: &Server, path: &str, bps: u64) -> NewDownload {
    NewDownload { url: srv.url(path), speed_limit: Some(bps), ..Default::default() }
}

fn fast(srv: &Server, path: &str) -> NewDownload {
    NewDownload { url: srv.url(path), ..Default::default() }
}

async fn until<F: std::future::Future<Output = bool>>(what: &str, secs: u64, mut f: impl FnMut() -> F) {
    let t = Instant::now();
    while !f().await {
        assert!(t.elapsed() < Duration::from_secs(secs), "timed out waiting for: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn status(m: &DownloadManager, id: i64) -> DownloadStatus {
    m.get_download(id).await.unwrap().status
}

async fn wait_status(m: &DownloadManager, id: i64, want: DownloadStatus) {
    until(&format!("download {id} -> {want:?}"), 25, || async { status(m, id).await == want }).await;
}

async fn stays(m: &DownloadManager, id: i64, want: DownloadStatus, ms: u64) {
    let t = Instant::now();
    while t.elapsed() < Duration::from_millis(ms) {
        assert_eq!(status(m, id).await, want, "state changed unexpectedly");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn qs(m: &DownloadManager) -> QueueStateDto {
    m.get_queue_state().await.unwrap()
}

async fn assert_file(m: &DownloadManager, id: i64, data: &[u8]) {
    let d = m.get_download(id).await.unwrap();
    assert_eq!(d.status, DownloadStatus::Completed);
    let got = std::fs::read(&d.destination).unwrap();
    assert!(got == data, "downloaded bytes differ from the source");
}

fn daily(start: &str, stop: &str) -> Schedule {
    Schedule { enabled: true, start: start.into(), stop: stop.into(), days: [true; 7] }
}

// ============================================================ regression guards

#[tokio::test]
async fn nothing_changes_until_the_user_configures_something() {
    let srv = serve(random_data(3 * MB, 1)).await;
    let (m, _e) = manager("defaults", 3).await;
    let mut rx = m.subscribe();

    let s = qs(&m).await;
    assert_eq!((s.max_active, s.max_connections), (3, 64), "config defaults are reported as-is");
    assert!(s.gate_open && !s.paused && s.held == 0);
    assert!(!s.schedule.enabled && s.schedule_open && s.next_change.is_none());
    assert_eq!(s.power.phase, PowerPhase::Off);
    assert!(s.power.action.is_none());

    let d = m.create_download(fast(&srv, "/a.bin")).await.unwrap();
    wait_status(&m, d.id, DownloadStatus::Completed).await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    let mut seen = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        seen.push(ev);
    }
    assert!(matches!(seen.first(), Some(ManagerEvent::Added { .. })), "first event is still Added");
    assert!(!seen.iter().any(|e| matches!(e, ManagerEvent::Queue { .. })), "ordinary downloads must not produce queue events");
    assert_file(&m, d.id, &random_data(3 * MB, 1)).await;
}

#[tokio::test]
async fn queued_never_started_downloads_are_not_auto_started_without_a_schedule() {
    let srv = serve(random_data(MB, 2)).await;
    let e = env("no-autostart");
    {
        let m = manager_at(&e, Opts::default()).await;
        let d = m.create_download(NewDownload { start: Some(false), ..fast(&srv, "/a.bin") }).await.unwrap();
        assert_eq!(d.status, DownloadStatus::Queued);
        m.shutdown().await;
    }
    let m = manager_at(&e, Opts::default()).await;
    tokio::time::sleep(Duration::from_millis(400)).await; // several queue-loop passes
    let all = m.list_downloads(Default::default()).await.unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].status, DownloadStatus::Queued, "start=false must stay queued as before");
}

#[tokio::test]
async fn every_new_command_rejects_unknown_fields_and_bad_types() {
    assert!(serde_json::from_str::<SetQueueConfig>(r#"{"maxActive":2}"#).is_ok());
    assert!(serde_json::from_str::<SetQueueConfig>(r#"{"maxActive":2,"evil":1}"#).is_err());
    assert!(serde_json::from_str::<SetQueueConfig>(r#"{"maxActive":-1}"#).is_err());
    assert!(serde_json::from_str::<PauseQueue>(r#"{"pauseRunning":true}"#).is_ok());
    assert!(serde_json::from_str::<PauseQueue>(r#"{"pauseRunning":true,"x":1}"#).is_err());
    assert!(serde_json::from_str::<PauseQueue>(r#"{}"#).is_err(), "the choice must be explicit");
    assert!(serde_json::from_str::<SetPowerAction>(r#"{"action":"shutdown"}"#).is_ok());
    assert!(serde_json::from_str::<SetPowerAction>(r#"{"action":null}"#).is_ok());
    assert!(serde_json::from_str::<SetPowerAction>(r#"{"action":"format-disk"}"#).is_err());
    assert!(serde_json::from_str::<SetPowerAction>(r#"{"action":"shutdown","now":true}"#).is_err());
}

// ============================================================ max active / max connections

#[tokio::test]
async fn max_active_limits_running_downloads_and_can_change_live() {
    let srv = serve(random_data(6 * MB, 3)).await;
    let (m, _e) = manager("max-active", 1).await;
    let mut ids = Vec::new();
    for n in 0..3 {
        ids.push(m.create_download(slow(&srv, &format!("/f{n}.bin"), 2 * MB as u64)).await.unwrap().id);
        tokio::time::sleep(Duration::from_millis(60)).await; // fix arrival order
    }
    wait_status(&m, ids[0], DownloadStatus::Downloading).await;
    let s = qs(&m).await;
    assert_eq!((s.max_active, s.running, s.waiting), (1, 1, 2));
    assert_eq!(status(&m, ids[1]).await, DownloadStatus::Queued);

    let s = m.set_queue_config(SetQueueConfig { max_active: Some(3), max_connections: None }).await.unwrap();
    assert_eq!(s.max_active, 3);
    for id in &ids {
        wait_status(&m, *id, DownloadStatus::Downloading).await; // the waiting two start at once
    }
    for id in &ids {
        wait_status(&m, *id, DownloadStatus::Completed).await;
    }
}

#[tokio::test]
async fn config_values_are_validated_and_a_bad_request_changes_nothing() {
    let (m, _e) = manager("cfg-validate", 3).await;
    for bad in [
        SetQueueConfig { max_active: Some(0), max_connections: None },
        SetQueueConfig { max_active: Some(33), max_connections: None },
        SetQueueConfig { max_active: None, max_connections: Some(0) },
        SetQueueConfig { max_active: None, max_connections: Some(65) }, // 64 is the most one download uses
        SetQueueConfig { max_active: Some(2), max_connections: Some(99) }, // one good, one bad: all rejected
    ] {
        assert!(matches!(m.set_queue_config(bad).await, Err(ManagerError::InvalidInput(_))));
    }
    let s = qs(&m).await;
    assert_eq!((s.max_active, s.max_connections), (3, 64));
    assert_eq!(m.set_queue_config(SetQueueConfig { max_active: None, max_connections: Some(64) }).await.unwrap().max_connections, 64);
    // An empty request is a valid no-op.
    assert!(m.set_queue_config(SetQueueConfig::default()).await.is_ok());
}

#[tokio::test]
async fn max_connections_caps_parallelism_for_new_downloads_and_can_be_raised() {
    let data = random_data(24 * MB, 4);
    let srv = serve(data.clone()).await;
    *srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(2);
    let e = env("max-conn");
    let m = manager_at(&e, Opts { adaptive: false, ..Default::default() }).await;

    m.set_queue_config(SetQueueConfig { max_active: None, max_connections: Some(2) }).await.unwrap();
    let a = m.create_download(fast(&srv, "/a.bin")).await.unwrap();
    wait_status(&m, a.id, DownloadStatus::Completed).await;
    let peak_capped = srv.state.peak_concurrent.load(Ordering::Relaxed);
    assert!((1..=2).contains(&peak_capped), "peak {peak_capped} exceeded the cap of 2");
    assert_file(&m, a.id, &data).await;

    // Control: the same download with a higher cap really does use more connections, so the
    // assertion above is sensitive to the setting rather than passing by accident.
    srv.state.peak_concurrent.store(0, Ordering::Relaxed);
    m.set_queue_config(SetQueueConfig { max_active: None, max_connections: Some(8) }).await.unwrap();
    let b = m.create_download(fast(&srv, "/b.bin")).await.unwrap();
    wait_status(&m, b.id, DownloadStatus::Completed).await;
    assert!(srv.state.peak_concurrent.load(Ordering::Relaxed) > 2, "raising the cap had no effect");
    assert_file(&m, b.id, &data).await;
}

// ============================================================ pause / resume queue

#[tokio::test]
async fn pause_queue_lets_running_finish_but_starts_nothing_new() {
    let data = random_data(6 * MB, 5);
    let srv = serve(data.clone()).await;
    let (m, _e) = manager("pause-soft", 1).await;
    let a = m.create_download(slow(&srv, "/a.bin", 3 * MB as u64)).await.unwrap();
    let b = m.create_download(fast(&srv, "/b.bin")).await.unwrap();
    let c = m.create_download(fast(&srv, "/c.bin")).await.unwrap();
    wait_status(&m, a.id, DownloadStatus::Downloading).await;

    let s = m.pause_queue(PauseQueue { pause_running: false }).await.unwrap();
    assert!(s.paused && !s.gate_open && s.held == 0);

    wait_status(&m, a.id, DownloadStatus::Completed).await; // the running one is left alone
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(status(&m, b.id).await, DownloadStatus::Queued, "nothing new may start while paused");
    assert_eq!(status(&m, c.id).await, DownloadStatus::Queued);
    let s = qs(&m).await;
    assert_eq!((s.running, s.waiting), (0, 2), "waiting downloads keep their place");

    let s = m.resume_queue().await.unwrap();
    assert!(!s.paused && s.gate_open);
    wait_status(&m, b.id, DownloadStatus::Completed).await;
    wait_status(&m, c.id, DownloadStatus::Completed).await;
    assert_file(&m, b.id, &data).await;
}

#[tokio::test]
async fn pause_queue_can_also_pause_running_downloads_and_resume_continues_them() {
    let data = random_data(8 * MB, 6);
    let srv = serve(data.clone()).await;
    let (m, _e) = manager("pause-hard", 1).await;
    let a = m.create_download(slow(&srv, "/a.bin", 3 * MB as u64)).await.unwrap();
    let b = m.create_download(slow(&srv, "/b.bin", 3 * MB as u64)).await.unwrap();
    until("a is transferring", 10, || async { m.get_download(a.id).await.unwrap().downloaded > MB as u64 }).await;

    let s = m.pause_queue(PauseQueue { pause_running: true }).await.unwrap();
    assert_eq!(s.held, 1, "only the running download is held; the waiting one just waits");
    assert_eq!(status(&m, a.id).await, DownloadStatus::Paused);
    assert_eq!(status(&m, b.id).await, DownloadStatus::Queued);
    let kept = m.get_download(a.id).await.unwrap().downloaded;
    assert!(kept > 0, "progress is kept, not discarded");
    stays(&m, a.id, DownloadStatus::Paused, 500).await;
    assert_eq!(status(&m, b.id).await, DownloadStatus::Queued);

    let s = m.resume_queue().await.unwrap();
    assert_eq!(s.held, 0);
    wait_status(&m, a.id, DownloadStatus::Completed).await;
    wait_status(&m, b.id, DownloadStatus::Completed).await;
    assert_file(&m, a.id, &data).await; // resumed data is byte-exact
    assert_file(&m, b.id, &data).await;
}

#[tokio::test]
async fn resumed_downloads_go_back_to_the_front_ahead_of_ones_that_never_started() {
    // One slot: `a` runs, `b` waits. After a pause/resume cycle it must still be `a` that runs;
    // the older waiter must not grab the freed slot first.
    let srv = serve(random_data(16 * MB, 30)).await;
    for round in 0..3 {
        let (m, _e) = manager(&format!("front-{round}"), 1).await;
        let a = m.create_download(slow(&srv, "/a.bin", 2 * MB as u64)).await.unwrap();
        let b = m.create_download(slow(&srv, "/b.bin", 2 * MB as u64)).await.unwrap();
        until("a transferring", 10, || async { m.get_download(a.id).await.unwrap().downloaded > 256 * 1024 }).await;
        m.pause_queue(PauseQueue { pause_running: true }).await.unwrap();
        wait_status(&m, a.id, DownloadStatus::Paused).await;
        m.resume_queue().await.unwrap();
        wait_status(&m, a.id, DownloadStatus::Downloading).await;
        assert_eq!(status(&m, b.id).await, DownloadStatus::Queued, "round {round}: b must keep waiting");
        m.cancel_download(a.id).await.unwrap();
        m.cancel_download(b.id).await.unwrap();
    }
}

#[tokio::test]
async fn when_the_schedule_window_reopens_the_interrupted_download_runs_before_older_waiters() {
    let srv = serve(random_data(16 * MB, 31)).await;
    let e = env("front-schedule");
    let clock = FakeClock::new(at(2, 23, 0));
    let m = manager_at(&e, Opts { slots: 1, clock: Some(clock.clone()), ..Default::default() }).await;
    m.set_schedule(daily("22:00", "07:00")).await.unwrap();
    let a = m.create_download(slow(&srv, "/a.bin", 2 * MB as u64)).await.unwrap();
    let b = m.create_download(slow(&srv, "/b.bin", 2 * MB as u64)).await.unwrap();
    until("a transferring", 10, || async { m.get_download(a.id).await.unwrap().downloaded > 256 * 1024 }).await;
    clock.set(at(3, 7, 0));
    wait_status(&m, a.id, DownloadStatus::Paused).await;
    clock.set(at(3, 22, 0));
    wait_status(&m, a.id, DownloadStatus::Downloading).await;
    assert_eq!(status(&m, b.id).await, DownloadStatus::Queued);
    m.cancel_download(a.id).await.unwrap();
    m.cancel_download(b.id).await.unwrap();
}

#[tokio::test]
async fn resume_queue_never_resumes_what_the_user_paused_or_cancelled() {
    let srv = serve(random_data(8 * MB, 7)).await;
    let (m, _e) = manager("user-intent", 3).await;
    let mine = m.create_download(slow(&srv, "/mine.bin", 2 * MB as u64)).await.unwrap();
    let queue = m.create_download(slow(&srv, "/queue.bin", 2 * MB as u64)).await.unwrap();
    let gone = m.create_download(slow(&srv, "/gone.bin", 2 * MB as u64)).await.unwrap();
    for id in [mine.id, queue.id, gone.id] {
        until("transferring", 10, || async { m.get_download(id).await.unwrap().downloaded > 0 }).await;
    }
    m.pause_download(mine.id).await.unwrap(); // the user's own pause

    let s = m.pause_queue(PauseQueue { pause_running: true }).await.unwrap();
    assert_eq!(s.held, 2, "the queue holds only what it paused itself");
    m.cancel_download(gone.id).await.unwrap(); // user cancels a held download meanwhile

    m.resume_queue().await.unwrap();
    wait_status(&m, queue.id, DownloadStatus::Downloading).await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(status(&m, mine.id).await, DownloadStatus::Paused, "user pause is respected");
    assert_eq!(status(&m, gone.id).await, DownloadStatus::Cancelled, "cancel is final");
    m.cancel_download(queue.id).await.unwrap();
}

#[tokio::test]
async fn pause_and_resume_queue_are_idempotent() {
    let (m, _e) = manager("idem", 3).await;
    for _ in 0..3 {
        m.pause_queue(PauseQueue { pause_running: true }).await.unwrap();
    }
    assert!(qs(&m).await.paused);
    for _ in 0..3 {
        m.resume_queue().await.unwrap();
    }
    let s = qs(&m).await;
    assert!(!s.paused && s.gate_open);
}

// ============================================================ regression: settings changed while running

#[tokio::test]
async fn priority_and_speed_limit_changed_on_a_running_download_stick() {
    // The engine saves its own copy of the record every checkpoint; a value changed from the UI
    // meanwhile must not be overwritten by that stale copy.
    let srv = serve(random_data(12 * MB, 40)).await;
    let (m, _e) = manager("running-settings", 3).await;
    let d = m.create_download(slow(&srv, "/a.bin", 2 * MB as u64)).await.unwrap();
    until("transferring", 10, || async { m.get_download(d.id).await.unwrap().downloaded > 256 * 1024 }).await;

    m.set_priority(d.id, download_manager::Priority::High).await.unwrap();
    m.set_speed_limit(download_manager::SetSpeedLimit {
        target: download_manager::SpeedTarget::Download { id: d.id },
        bytes_per_sec: Some(3 * MB as u64),
    })
    .await
    .unwrap();
    for _ in 0..8 {
        // 8 x 250 ms spans several 50 ms checkpoints
        tokio::time::sleep(Duration::from_millis(250)).await;
        let now = m.get_download(d.id).await.unwrap();
        assert_eq!(now.priority, download_manager::Priority::High, "priority was reverted by a checkpoint");
        assert_eq!(now.speed_limit, Some(3 * MB as u64), "speed limit was reverted by a checkpoint");
    }
    m.cancel_download(d.id).await.unwrap();
}

// ============================================================ scheduler

#[tokio::test]
async fn schedule_holds_downloads_outside_the_window_and_starts_them_when_it_opens() {
    let data = random_data(3 * MB, 8);
    let srv = serve(data.clone()).await;
    let e = env("sched-open");
    let clock = FakeClock::new(at(2, 12, 0)); // Monday noon
    let m = manager_at(&e, Opts { clock: Some(clock.clone()), ..Default::default() }).await;
    let mut rx = m.subscribe();

    let s = m.set_schedule(daily("22:00", "07:00")).await.unwrap();
    assert!(s.schedule.enabled && !s.schedule_open && !s.gate_open);
    assert_eq!(s.next_change.as_deref(), Some("2026-03-02T22:00"));

    let d = m.create_download(fast(&srv, "/a.bin")).await.unwrap();
    stays(&m, d.id, DownloadStatus::Queued, 600).await; // outside the window: waits

    clock.set(at(2, 22, 0));
    wait_status(&m, d.id, DownloadStatus::Completed).await;
    assert_file(&m, d.id, &data).await;
    let s = qs(&m).await;
    assert!(s.schedule_open && s.gate_open);
    assert_eq!(s.next_change.as_deref(), Some("2026-03-03T07:00"));

    let mut opened = false;
    while let Ok(ev) = rx.try_recv() {
        if let ManagerEvent::Queue { queue } = ev {
            opened |= queue.gate_open && queue.schedule_open;
        }
    }
    assert!(opened, "the UI is told when the window opens");
}

#[tokio::test]
async fn schedule_stop_pauses_running_downloads_and_the_next_window_resumes_them() {
    let data = random_data(8 * MB, 9);
    let srv = serve(data.clone()).await;
    let e = env("sched-stop");
    let clock = FakeClock::new(at(2, 23, 0)); // inside the window
    let m = manager_at(&e, Opts { clock: Some(clock.clone()), ..Default::default() }).await;
    m.set_schedule(daily("22:00", "07:00")).await.unwrap();
    assert!(qs(&m).await.gate_open);

    let d = m.create_download(slow(&srv, "/a.bin", 2 * MB as u64)).await.unwrap();
    until("transferring", 10, || async { m.get_download(d.id).await.unwrap().downloaded > 512 * 1024 }).await;

    clock.set(at(3, 7, 0)); // "Stop at 07:00"
    wait_status(&m, d.id, DownloadStatus::Paused).await;
    // The download is saved as paused a moment before the queue writes it down as one of its own (both under the
    // queue's lock, so nothing can let it go in between): wait for that, do not read it at once.
    until("the queue to hold it", 10, || async {
        let s = qs(&m).await;
        !s.gate_open && s.held == 1
    })
    .await;
    let kept = m.get_download(d.id).await.unwrap().downloaded;
    assert!(kept > 0 && kept < data.len() as u64);
    stays(&m, d.id, DownloadStatus::Paused, 500).await;

    clock.set(at(3, 22, 0)); // next evening
    wait_status(&m, d.id, DownloadStatus::Completed).await;
    assert_file(&m, d.id, &data).await;
    assert_eq!(qs(&m).await.held, 0);
}

#[tokio::test]
async fn disabling_the_schedule_reopens_the_queue_immediately() {
    let srv = serve(random_data(MB, 10)).await;
    let e = env("sched-disable");
    let clock = FakeClock::new(at(2, 12, 0));
    let m = manager_at(&e, Opts { clock: Some(clock), ..Default::default() }).await;
    m.set_schedule(daily("22:00", "07:00")).await.unwrap();
    let d = m.create_download(fast(&srv, "/a.bin")).await.unwrap();
    stays(&m, d.id, DownloadStatus::Queued, 300).await;

    let mut off = daily("22:00", "07:00");
    off.enabled = false;
    m.set_schedule(off).await.unwrap();
    wait_status(&m, d.id, DownloadStatus::Completed).await;
}

#[tokio::test]
async fn invalid_schedules_are_rejected_without_side_effects() {
    let e = env("sched-invalid");
    let m = manager_at(&e, Opts { clock: Some(FakeClock::new(at(2, 12, 0))), ..Default::default() }).await;
    let mut none = daily("22:00", "07:00");
    none.days = [false; 7];
    for bad in [daily("22:00", "22:00"), daily("25:00", "07:00"), daily("22:00", "7:00"), daily("ab:cd", "07:00"), none] {
        assert!(matches!(m.set_schedule(bad).await, Err(ManagerError::InvalidInput(_))));
    }
    let s = qs(&m).await;
    assert!(!s.schedule.enabled && s.gate_open, "a rejected schedule must leave the queue untouched");
}

#[tokio::test]
async fn manual_pause_and_schedule_combine_correctly() {
    let srv = serve(random_data(MB, 11)).await;
    let e = env("combine");
    let clock = FakeClock::new(at(2, 23, 0)); // inside the window
    let m = manager_at(&e, Opts { clock: Some(clock.clone()), ..Default::default() }).await;
    m.set_schedule(daily("22:00", "07:00")).await.unwrap();
    m.pause_queue(PauseQueue { pause_running: false }).await.unwrap();
    let d = m.create_download(fast(&srv, "/a.bin")).await.unwrap();

    // Window open but the user paused: still closed.
    stays(&m, d.id, DownloadStatus::Queued, 400).await;
    // Window closes and reopens while paused: still the user's pause that decides.
    clock.set(at(3, 8, 0));
    tokio::time::sleep(Duration::from_millis(200)).await;
    clock.set(at(3, 22, 30));
    stays(&m, d.id, DownloadStatus::Queued, 400).await;
    m.resume_queue().await.unwrap();
    wait_status(&m, d.id, DownloadStatus::Completed).await;
}

// ============================================================ persistence across restarts

#[tokio::test]
async fn queue_settings_survive_a_restart_and_the_gate_starts_closed_when_paused() {
    let srv = serve(random_data(MB, 12)).await;
    let e = env("persist");
    let clock = FakeClock::new(at(2, 12, 0));
    {
        let m = manager_at(&e, Opts { clock: Some(clock.clone()), ..Default::default() }).await;
        m.set_queue_config(SetQueueConfig { max_active: Some(2), max_connections: Some(4) }).await.unwrap();
        m.set_schedule(daily("09:00", "17:00")).await.unwrap();
        m.pause_queue(PauseQueue { pause_running: false }).await.unwrap();
        m.shutdown().await;
    }
    let m = manager_at(&e, Opts { clock: Some(clock), slots: 3, ..Default::default() }).await;
    let s = qs(&m).await;
    assert_eq!((s.max_active, s.max_connections), (2, 4), "saved values win over config defaults");
    assert!(s.paused && !s.gate_open, "closed from the very first moment, before any evaluation");
    assert_eq!((s.schedule.start.as_str(), s.schedule.stop.as_str()), ("09:00", "17:00"));

    let d = m.create_download(fast(&srv, "/a.bin")).await.unwrap();
    stays(&m, d.id, DownloadStatus::Queued, 400).await;
    m.resume_queue().await.unwrap();
    wait_status(&m, d.id, DownloadStatus::Completed).await;
}

#[tokio::test]
async fn held_downloads_survive_a_restart_and_resume_with_the_queue() {
    let data = random_data(8 * MB, 13);
    let srv = serve(data.clone()).await;
    let e = env("held-restart");
    let id;
    {
        let m = manager_at(&e, Opts::default()).await;
        id = m.create_download(slow(&srv, "/a.bin", 3 * MB as u64)).await.unwrap().id;
        until("transferring", 10, || async { m.get_download(id).await.unwrap().downloaded > MB as u64 }).await;
        assert_eq!(m.pause_queue(PauseQueue { pause_running: true }).await.unwrap().held, 1);
        m.shutdown().await;
    }
    let m = manager_at(&e, Opts::default()).await;
    let s = qs(&m).await;
    assert!(s.paused && s.held == 1);
    stays(&m, id, DownloadStatus::Paused, 300).await;
    m.resume_queue().await.unwrap();
    wait_status(&m, id, DownloadStatus::Completed).await;
    assert_file(&m, id, &data).await;
}

#[tokio::test]
async fn with_a_schedule_a_restart_inside_the_window_starts_the_waiting_queue() {
    let data = random_data(2 * MB, 14);
    let srv = serve(data.clone()).await;
    let e = env("restart-window");
    let clock = FakeClock::new(at(2, 12, 0));
    let id;
    {
        let m = manager_at(&e, Opts { clock: Some(clock.clone()), ..Default::default() }).await;
        id = m.create_download(NewDownload { start: Some(false), ..fast(&srv, "/a.bin") }).await.unwrap().id;
        m.set_schedule(daily("09:00", "17:00")).await.unwrap(); // noon: open
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(status(&m, id).await, DownloadStatus::Queued, "enabling a schedule does not start it");
        m.shutdown().await;
    }
    let m = manager_at(&e, Opts { clock: Some(clock), ..Default::default() }).await;
    wait_status(&m, id, DownloadStatus::Completed).await; // launched inside the window: it runs
    assert_file(&m, id, &data).await;
}

// ============================================================ power actions

#[tokio::test]
async fn the_computer_is_kept_awake_exactly_while_downloading_unless_the_user_turns_it_off() {
    let srv = serve(random_data(2 * MB, 17)).await;
    let e = env("keep-awake");
    let system = fake_power();
    let m = manager_at(&e, Opts { power: Some(system.clone()), ..Default::default() }).await;
    let s = qs(&m).await.power;
    assert!(s.keep_awake && !s.kept_awake && s.keep_awake_available, "on by default, nothing held while idle");

    let d = m.create_download(slow(&srv, "/a.bin", MB as u64)).await.unwrap();
    until("held while downloading", 5, || async { system.awake_holds() == 1 }).await;
    assert!(qs(&m).await.power.kept_awake);
    wait_status(&m, d.id, DownloadStatus::Completed).await;
    until("released when done", 5, || async { system.awake_holds() == 0 }).await;
    assert!(!qs(&m).await.power.kept_awake);

    // Turned off: a download no longer holds the computer awake; turned off during one, it is released at once.
    let d = m.create_download(slow(&srv, "/b.bin", MB as u64)).await.unwrap();
    until("held", 5, || async { system.awake_holds() == 1 }).await;
    let s = m.set_keep_awake(false).await.unwrap();
    assert!(!s.power.keep_awake && !s.power.kept_awake);
    assert_eq!(system.awake_holds(), 0);
    wait_status(&m, d.id, DownloadStatus::Completed).await;
    let d = m.create_download(slow(&srv, "/c.bin", MB as u64)).await.unwrap();
    wait_status(&m, d.id, DownloadStatus::Downloading).await;
    stays(&m, d.id, DownloadStatus::Downloading, 600).await;
    assert_eq!(system.awake_holds(), 0, "off means off");
    assert!(!qs(&m).await.power.keep_awake, "the choice is saved");
    // Exactly one hold was taken per busy period, never one per download.
    let takes = system.calls().iter().filter(|c| matches!(c, system_integration::SystemCall::KeepAwake(_))).count();
    assert_eq!(takes, 2);
}

#[tokio::test]
async fn power_action_is_off_by_default_and_never_runs_unless_armed() {
    let srv = serve(random_data(MB, 15)).await;
    let e = env("power-off");
    let power = fake_power();
    let m = manager_at(&e, Opts { power: Some(power.clone()), grace: Duration::from_millis(100), ..Default::default() }).await;
    let d = m.create_download(fast(&srv, "/a.bin")).await.unwrap();
    wait_status(&m, d.id, DownloadStatus::Completed).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(power.power_calls().is_empty(), "nothing may happen without explicit configuration");
    assert_eq!(qs(&m).await.power.phase, PowerPhase::Off);
}

#[tokio::test]
async fn armed_action_runs_once_after_everything_finished_with_a_visible_countdown() {
    let srv = serve(random_data(3 * MB, 16)).await;
    let e = env("power-fire");
    let power = fake_power();
    let m = manager_at(&e, Opts { power: Some(power.clone()), grace: Duration::from_millis(600), ..Default::default() }).await;
    let mut rx = m.subscribe();

    let s = m.set_power_action(SetPowerAction { action: Some(PowerAction::Shutdown) }).await.unwrap();
    assert_eq!((s.power.action, s.power.phase), (Some(PowerAction::Shutdown), PowerPhase::Armed));
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(power.power_calls().is_empty(), "armed with nothing downloaded must not fire");

    let a = m.create_download(slow(&srv, "/a.bin", 2 * MB as u64)).await.unwrap();
    let b = m.create_download(slow(&srv, "/b.bin", 3 * MB as u64)).await.unwrap();
    wait_status(&m, a.id, DownloadStatus::Completed).await;
    assert!(power.power_calls().is_empty(), "b is still running");
    wait_status(&m, b.id, DownloadStatus::Completed).await;

    until("the action runs", 5, || async { !power.power_calls().is_empty() }).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(power.power_calls(), vec![PowerAction::Shutdown], "exactly once");
    assert_eq!(qs(&m).await.power.phase, PowerPhase::Off, "one-shot: disarmed after firing");

    let mut phases = Vec::new();
    let mut deadline_seen = false;
    while let Ok(ev) = rx.try_recv() {
        if let ManagerEvent::Queue { queue } = ev {
            deadline_seen |= queue.power.countdown_ends_at_ms.is_some();
            if phases.last() != Some(&queue.power.phase) {
                phases.push(queue.power.phase);
            }
        }
    }
    assert!(phases.contains(&PowerPhase::Countdown), "a countdown was announced: {phases:?}");
    assert!(deadline_seen, "the UI gets the moment the action will run");
    assert_eq!(phases.last(), Some(&PowerPhase::Off));
}

#[tokio::test]
async fn cancelling_during_the_countdown_prevents_the_action() {
    let srv = serve(random_data(MB, 17)).await;
    let e = env("power-cancel");
    let power = fake_power();
    let m = manager_at(&e, Opts { power: Some(power.clone()), grace: Duration::from_millis(1500), ..Default::default() }).await;
    m.set_power_action(SetPowerAction { action: Some(PowerAction::Hibernate) }).await.unwrap();
    let d = m.create_download(fast(&srv, "/a.bin")).await.unwrap();
    wait_status(&m, d.id, DownloadStatus::Completed).await;
    until("countdown", 5, || async { qs(&m).await.power.phase == PowerPhase::Countdown }).await;

    let s = m.set_power_action(SetPowerAction { action: None }).await.unwrap();
    assert_eq!(s.power.phase, PowerPhase::Off);
    tokio::time::sleep(Duration::from_millis(2000)).await;
    assert!(power.power_calls().is_empty(), "a cancelled countdown must never run");
}

#[tokio::test]
async fn a_new_download_during_the_countdown_aborts_it_and_the_action_waits_for_that_one_too() {
    let srv = serve(random_data(4 * MB, 18)).await;
    let e = env("power-abort");
    let power = fake_power();
    let m = manager_at(&e, Opts { power: Some(power.clone()), grace: Duration::from_millis(1200), ..Default::default() }).await;
    m.set_power_action(SetPowerAction { action: Some(PowerAction::Sleep) }).await.unwrap();
    let a = m.create_download(fast(&srv, "/a.bin")).await.unwrap();
    wait_status(&m, a.id, DownloadStatus::Completed).await;
    until("countdown", 5, || async { qs(&m).await.power.phase == PowerPhase::Countdown }).await;

    let b = m.create_download(slow(&srv, "/b.bin", 2 * MB as u64)).await.unwrap(); // ~2 s
    until("countdown aborted", 5, || async { qs(&m).await.power.phase == PowerPhase::Armed }).await;
    assert!(power.power_calls().is_empty());
    wait_status(&m, b.id, DownloadStatus::Completed).await;
    until("fires after b", 8, || async { !power.power_calls().is_empty() }).await;
    assert_eq!(power.power_calls(), vec![PowerAction::Sleep]);
}

#[tokio::test]
async fn pausing_a_download_yourself_does_not_count_as_finished() {
    let srv = serve(random_data(6 * MB, 19)).await;
    let e = env("power-userpause");
    let power = fake_power();
    let m = manager_at(&e, Opts { power: Some(power.clone()), grace: Duration::from_millis(200), ..Default::default() }).await;
    let d = m.create_download(slow(&srv, "/a.bin", 2 * MB as u64)).await.unwrap();
    until("transferring", 10, || async { m.get_download(d.id).await.unwrap().downloaded > 0 }).await;
    m.set_power_action(SetPowerAction { action: Some(PowerAction::Shutdown) }).await.unwrap();

    m.pause_download(d.id).await.unwrap();
    tokio::time::sleep(Duration::from_millis(900)).await;
    assert!(power.power_calls().is_empty(), "an unfinished, user-paused download must not trigger shutdown");
    assert_eq!(qs(&m).await.power.phase, PowerPhase::Armed);

    m.resume_download(d.id).await.unwrap();
    wait_status(&m, d.id, DownloadStatus::Completed).await;
    until("fires after completion", 5, || async { !power.power_calls().is_empty() }).await;
}

#[tokio::test]
async fn downloads_held_by_the_queue_keep_the_power_action_from_firing() {
    let srv = serve(random_data(8 * MB, 20)).await;
    let e = env("power-held");
    let power = fake_power();
    let m = manager_at(&e, Opts { power: Some(power.clone()), grace: Duration::from_millis(200), ..Default::default() }).await;
    let d = m.create_download(slow(&srv, "/a.bin", 3 * MB as u64)).await.unwrap();
    until("transferring", 10, || async { m.get_download(d.id).await.unwrap().downloaded > 0 }).await;
    m.set_power_action(SetPowerAction { action: Some(PowerAction::Shutdown) }).await.unwrap();
    m.pause_queue(PauseQueue { pause_running: true }).await.unwrap();
    tokio::time::sleep(Duration::from_millis(900)).await;
    assert!(power.power_calls().is_empty());
    m.resume_queue().await.unwrap();
    wait_status(&m, d.id, DownloadStatus::Completed).await;
    until("fires", 5, || async { !power.power_calls().is_empty() }).await;
}

#[tokio::test]
async fn unsupported_actions_are_rejected_and_executor_errors_are_reported() {
    let srv = serve(random_data(MB, 21)).await;
    let e = env("power-errors");
    let only_sleep = Arc::new(DryRunSystem::new().with_power(vec![PowerAction::Sleep], true));
    let m = manager_at(&e, Opts { power: Some(only_sleep.clone()), grace: Duration::from_millis(100), ..Default::default() }).await;

    assert!(matches!(
        m.set_power_action(SetPowerAction { action: Some(PowerAction::Hibernate) }).await,
        Err(ManagerError::InvalidInput(_))
    ));
    assert_eq!(qs(&m).await.power.phase, PowerPhase::Off, "a rejected request arms nothing");
    assert_eq!(qs(&m).await.power.supported, vec![PowerAction::Sleep]);

    m.set_power_action(SetPowerAction { action: Some(PowerAction::Sleep) }).await.unwrap();
    let d = m.create_download(fast(&srv, "/a.bin")).await.unwrap();
    wait_status(&m, d.id, DownloadStatus::Completed).await;
    until("attempted", 5, || async { !only_sleep.power_calls().is_empty() }).await;
    until("error reported", 5, || async { qs(&m).await.power.last_error.is_some() }).await;
    let s = qs(&m).await;
    assert_eq!(s.power.phase, PowerPhase::Off, "a failed attempt is not retried");
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(only_sleep.power_calls().len(), 1);
}

#[tokio::test]
async fn shutting_the_manager_down_disarms_everything() {
    let srv = serve(random_data(MB, 22)).await;
    let e = env("power-shutdown");
    let power = fake_power();
    let m = manager_at(&e, Opts { power: Some(power.clone()), grace: Duration::from_millis(100), ..Default::default() }).await;
    m.set_power_action(SetPowerAction { action: Some(PowerAction::Shutdown) }).await.unwrap();
    let d = m.create_download(slow(&srv, "/a.bin", 512 * 1024)).await.unwrap();
    until("transferring", 10, || async { m.get_download(d.id).await.unwrap().downloaded > 0 }).await;
    m.shutdown().await; // the app is exiting: everything paused
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(power.power_calls().is_empty(), "exiting the app must not trigger a power action");

    // A restart never inherits an armed action.
    let m2 = manager_at(&e, Opts { power: Some(power.clone()), ..Default::default() }).await;
    assert_eq!(qs(&m2).await.power.phase, PowerPhase::Off);
}
