//! Smart Download Queue: several queues with rules (time window, start time, idle, connection, busy line,
//! big files), per-download start times, the user's order, the express lane, and persistence. Includes the
//! regression guard: with no queue configured everything behaves as before.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use download_manager::{
    Clock, ConnectionRule, DownloadManager, ManagerConfig, ManagerEvent, NewDownload, PlaceDownload, Priority, QueueAction, QueueGate,
    QueueRules, QueueStep, QueueWhen, QueuesDto, SaveQueue, SetSmartThrottle, StartWhen, SystemHandle, ThrottleSignals, MAIN_QUEUE,
};
use downloader_core::{DownloadStatus, RetryPolicy};
use scheduler::chrono::{NaiveDate, NaiveDateTime};
use system_integration::{ConnectionKind, DryRunSystem, NetworkState};
use velox_integration_tests::{random_data, serve, Server};

const MB: usize = 1024 * 1024;

struct Env {
    db: PathBuf,
    root: PathBuf,
}

fn env(name: &str) -> Env {
    let base = std::env::temp_dir().join(format!("velox-sq-{}-{name}", std::process::id()));
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
}

struct Rig {
    m: DownloadManager,
    clock: FakeClock,
    sys: Arc<DryRunSystem>,
}

async fn rig_at(e: &Env, slots: usize, clock: FakeClock, sys: Arc<DryRunSystem>) -> DownloadManager {
    let mut c = ManagerConfig::new(e.db.clone(), e.root.clone());
    c.max_concurrent_downloads = slots;
    c.queue_poll = Duration::from_millis(30);
    c.download.min_segment_size = 64 * 1024;
    c.download.checkpoint_interval = Duration::from_millis(50);
    c.download.retry = RetryPolicy { max_retries: 1, max_throttle_retries: 1, ..Default::default() };
    let cl = clock.0.clone();
    c.clock = Clock(Arc::new(move || *cl.lock().unwrap()));
    c.system = SystemHandle(sys);
    DownloadManager::new(c).await.unwrap()
}

async fn rig(name: &str, slots: usize) -> (Rig, Env) {
    let e = env(name);
    let clock = FakeClock::new(at(2, 12, 0));
    let sys = Arc::new(DryRunSystem::new());
    let m = rig_at(&e, slots, clock.clone(), sys.clone()).await;
    (Rig { m, clock, sys }, e)
}

fn slow(srv: &Server, path: &str, bps: u64) -> NewDownload {
    NewDownload { url: srv.url(path), speed_limit: Some(bps), ..Default::default() }
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

async fn add_queue(m: &DownloadManager, name: &str, rules: QueueRules) -> i64 {
    let q = m.save_queue(SaveQueue { id: None, name: name.into(), rules }).await.unwrap();
    q.queues.iter().find(|x| x.name == name).unwrap().id
}

async fn gate(m: &DownloadManager, id: i64) -> QueueGate {
    m.get_queues().await.unwrap().queues.iter().find(|q| q.id == id).unwrap().gate
}

fn night() -> QueueRules {
    QueueRules { when: QueueWhen::Between { start: "23:00".into(), stop: "07:00".into(), days: [true; 7] }, ..Default::default() }
}

#[tokio::test]
async fn with_no_queue_configured_everything_runs_as_before() {
    let srv = serve(random_data(MB, 1)).await;
    let (r, _e) = rig("plain", 3).await;
    let q = r.m.get_queues().await.unwrap();
    assert_eq!(q.queues.len(), 1);
    assert_eq!((q.queues[0].id, q.queues[0].name.as_str(), q.queues[0].gate), (MAIN_QUEUE, "", QueueGate::Open));
    assert!(q.queues[0].rules == QueueRules::default() && q.queues[0].spans.is_empty());
    let d = r.m.create_download(slow(&srv, "/a.bin", 4 * MB as u64)).await.unwrap();
    assert_eq!((d.queue_id, d.start_at.as_deref(), d.retrying), (MAIN_QUEUE, None, false));
    wait_status(&r.m, d.id, DownloadStatus::Completed).await;
    // "Download later" still means: nothing starts it but the user.
    let later = r.m.create_download(NewDownload { url: srv.url("/later.bin"), start: Some(false), ..Default::default() }).await.unwrap();
    stays(&r.m, later.id, DownloadStatus::Queued, 300).await;
    assert!(r.m.get_download(later.id).await.unwrap().idle);
}

#[tokio::test]
async fn a_night_queue_holds_its_downloads_until_the_window_opens_and_pauses_them_when_it_ends() {
    let srv = serve(random_data(4 * MB, 2)).await;
    let (r, _e) = rig("night", 3).await;
    let night = add_queue(&r.m, "Night", night()).await;
    let qs = r.m.get_queues().await.unwrap();
    let nq = qs.queues.iter().find(|q| q.id == night).unwrap();
    assert_eq!((nq.gate, nq.opens_at.as_deref()), (QueueGate::Time, Some("2026-03-02T23:00")));
    assert_eq!(nq.spans.len(), 1, "the timeline shows tonight's window");
    // Added to the night queue at noon: it waits; the main queue keeps working.
    let d = r.m.create_download(NewDownload { queue_id: Some(night), ..slow(&srv, "/n.bin", 256 * 1024) }).await.unwrap();
    assert_eq!(r.m.get_download(d.id).await.unwrap().queue_id, night);
    let other = r.m.create_download(slow(&srv, "/main.bin", 8 * MB as u64)).await.unwrap();
    wait_status(&r.m, other.id, DownloadStatus::Completed).await;
    stays(&r.m, d.id, DownloadStatus::Queued, 300).await;
    // 23:00: it runs.
    r.clock.set(at(2, 23, 0));
    wait_status(&r.m, d.id, DownloadStatus::Downloading).await;
    assert_eq!(gate(&r.m, night).await, QueueGate::Open);
    // 07:00: it pauses (kept), and resumes the next night.
    r.clock.set(at(3, 7, 0));
    wait_status(&r.m, d.id, DownloadStatus::Paused).await;
    r.clock.set(at(3, 23, 0));
    wait_status(&r.m, d.id, DownloadStatus::Downloading).await;
    r.m.cancel_download(d.id).await.unwrap();
}

#[tokio::test]
async fn a_start_time_queue_starts_what_was_added_for_later_and_closes_when_done() {
    let srv = serve(random_data(MB, 3)).await;
    let (r, _e) = rig("startat", 3).await;
    let rules = QueueRules { when: QueueWhen::StartAt { time: "02:00".into(), days: [true; 7] }, ..Default::default() };
    let q = add_queue(&r.m, "At two", rules).await;
    let d =
        r.m.create_download(NewDownload { queue_id: Some(q), start: Some(false), ..slow(&srv, "/two.bin", 8 * MB as u64) }).await.unwrap();
    stays(&r.m, d.id, DownloadStatus::Queued, 200).await;
    r.clock.set(at(3, 2, 0));
    wait_status(&r.m, d.id, DownloadStatus::Completed).await;
    // Done: closed until tomorrow, so something new waits.
    until("queue closes when empty", 10, || async { gate(&r.m, q).await == QueueGate::Time }).await;
    let next = r.m.get_queues().await.unwrap().queues.into_iter().find(|x| x.id == q).unwrap();
    assert_eq!(next.opens_at.as_deref(), Some("2026-03-04T02:00"));
}

#[tokio::test]
async fn a_download_with_its_own_start_time_waits_for_it_and_can_be_started_now() {
    let srv = serve(random_data(MB, 4)).await;
    let (r, _e) = rig("own-time", 3).await;
    let d =
        r.m.create_download(NewDownload { start_at: Some("2026-03-02T14:30".into()), ..slow(&srv, "/later.bin", 8 * MB as u64) })
            .await
            .unwrap();
    assert_eq!(d.start_at.as_deref(), Some("2026-03-02T14:30"));
    stays(&r.m, d.id, DownloadStatus::Queued, 250).await;
    r.clock.set(at(2, 14, 30));
    wait_status(&r.m, d.id, DownloadStatus::Completed).await;
    assert_eq!(r.m.get_download(d.id).await.unwrap().start_at, None, "used up");

    // Scheduled, then started now by hand.
    let e = r.m.create_download(NewDownload { start: Some(false), ..slow(&srv, "/e.bin", 8 * MB as u64) }).await.unwrap();
    let placed =
        r.m.place_download(PlaceDownload {
            id: e.id,
            queue_id: None,
            start: Some(StartWhen::At { at: "2026-03-05T09:00".into() }),
            step: None,
        })
        .await
        .unwrap();
    assert_eq!(placed.start_at.as_deref(), Some("2026-03-05T09:00"));
    r.m.place_download(PlaceDownload { id: e.id, queue_id: None, start: Some(StartWhen::Now), step: None }).await.unwrap();
    wait_status(&r.m, e.id, DownloadStatus::Completed).await;
    assert_eq!(r.m.get_download(e.id).await.unwrap().start_at, None);

    // Bad times are refused.
    for bad in ["tomorrow", "2026-03-02 14:30", "2030-01-01T00:00"] {
        let err = r.m.create_download(NewDownload { start_at: Some(bad.into()), ..slow(&srv, "/x.bin", 8 * MB as u64) }).await;
        assert!(err.is_err(), "{bad} accepted");
    }
}

#[tokio::test]
async fn pausing_a_queue_pauses_its_running_downloads_and_start_brings_everything_back() {
    let srv = serve(random_data(4 * MB, 5)).await;
    let (r, _e) = rig("pause", 3).await;
    let q = add_queue(&r.m, "Work", QueueRules::default()).await;
    let a = r.m.create_download(NewDownload { queue_id: Some(q), ..slow(&srv, "/a.bin", 256 * 1024) }).await.unwrap();
    let b = r.m.create_download(NewDownload { queue_id: Some(q), start: Some(false), ..slow(&srv, "/b.bin", 256 * 1024) }).await.unwrap();
    let main = r.m.create_download(slow(&srv, "/m.bin", 256 * 1024)).await.unwrap();
    wait_status(&r.m, a.id, DownloadStatus::Downloading).await;
    r.m.queue_action(q, QueueAction::Pause).await.unwrap();
    wait_status(&r.m, a.id, DownloadStatus::Paused).await;
    assert_eq!(gate(&r.m, q).await, QueueGate::Paused);
    assert_eq!(status(&r.m, main.id).await, DownloadStatus::Downloading, "other queues are not touched");
    r.m.queue_action(q, QueueAction::Start).await.unwrap();
    wait_status(&r.m, a.id, DownloadStatus::Downloading).await;
    wait_status(&r.m, b.id, DownloadStatus::Downloading).await;
    // Priority for the whole queue.
    r.m.queue_action(q, QueueAction::Priority { priority: Priority::High }).await.unwrap();
    assert_eq!(r.m.get_download(a.id).await.unwrap().priority, Priority::High);
    assert_eq!(r.m.get_download(main.id).await.unwrap().priority, Priority::Normal);
    for id in [a.id, b.id, main.id] {
        r.m.cancel_download(id).await.unwrap();
    }
}

#[tokio::test]
async fn only_when_idle_and_only_on_wifi_follow_what_the_computer_says() {
    let srv = serve(random_data(4 * MB, 6)).await;
    let (r, _e) = rig("idle-wifi", 3).await;
    r.sys.set_user_idle(Some(Duration::from_secs(5)));
    let away = add_queue(&r.m, "Away", QueueRules { only_when_idle: true, ..Default::default() }).await;
    let d = r.m.create_download(NewDownload { queue_id: Some(away), ..slow(&srv, "/i.bin", 256 * 1024) }).await.unwrap();
    until("waits for idle", 10, || async { gate(&r.m, away).await == QueueGate::NotIdle }).await;
    stays(&r.m, d.id, DownloadStatus::Queued, 200).await;
    r.sys.set_user_idle(Some(Duration::from_secs(300)));
    wait_status(&r.m, d.id, DownloadStatus::Downloading).await;
    r.m.cancel_download(d.id).await.unwrap();

    r.sys.set_network(NetworkState { online: Some(true), kind: Some(ConnectionKind::Wired) });
    let wifi = add_queue(&r.m, "Wi-Fi", QueueRules { connection: ConnectionRule::Wifi, ..Default::default() }).await;
    let w = r.m.create_download(NewDownload { queue_id: Some(wifi), ..slow(&srv, "/w.bin", 256 * 1024) }).await.unwrap();
    until("wrong connection", 10, || async { gate(&r.m, wifi).await == QueueGate::Connection }).await;
    stays(&r.m, w.id, DownloadStatus::Queued, 200).await;
    r.sys.set_network(NetworkState { online: Some(true), kind: Some(ConnectionKind::Wifi) });
    wait_status(&r.m, w.id, DownloadStatus::Downloading).await;
    let q = r.m.get_queues().await.unwrap();
    assert_eq!((q.online, q.connection, q.idle_known), (Some(true), Some(ConnectionKind::Wifi), true));
    r.m.cancel_download(w.id).await.unwrap();
}

#[tokio::test]
async fn nothing_new_starts_without_a_network_and_it_goes_on_when_the_network_is_back() {
    let srv = serve(random_data(MB, 7)).await;
    let (r, _e) = rig("offline", 3).await;
    r.sys.set_network(NetworkState { online: Some(false), kind: None });
    // Something to download, so the network is looked at.
    r.m.create_download(NewDownload { start: Some(false), ..slow(&srv, "/x.bin", 8 * MB as u64) }).await.unwrap();
    until("offline seen", 10, || async { gate(&r.m, MAIN_QUEUE).await == QueueGate::Offline }).await;
    let d = r.m.create_download(slow(&srv, "/o.bin", 8 * MB as u64)).await.unwrap();
    stays(&r.m, d.id, DownloadStatus::Queued, 250).await;
    r.sys.set_network(NetworkState { online: Some(true), kind: Some(ConnectionKind::Wired) });
    wait_status(&r.m, d.id, DownloadStatus::Completed).await;
}

#[tokio::test]
async fn moving_downloads_up_and_down_changes_which_one_starts_next() {
    let srv = serve(random_data(MB, 8)).await;
    let (r, _e) = rig("order", 1).await;
    let blocker = r.m.create_download(slow(&srv, "/blocker.bin", 128 * 1024)).await.unwrap();
    wait_status(&r.m, blocker.id, DownloadStatus::Downloading).await;
    let a = r.m.create_download(slow(&srv, "/a.bin", 8 * MB as u64)).await.unwrap();
    let b = r.m.create_download(slow(&srv, "/b.bin", 8 * MB as u64)).await.unwrap();
    let c = r.m.create_download(slow(&srv, "/c.bin", 8 * MB as u64)).await.unwrap();
    // c to the top, then a down one place: c, b, a.
    r.m.place_download(PlaceDownload { id: c.id, queue_id: None, start: None, step: Some(QueueStep::Top) }).await.unwrap();
    let mut moved = r.m.subscribe();
    r.m.place_download(PlaceDownload { id: a.id, queue_id: None, start: None, step: Some(QueueStep::Down) }).await.unwrap();
    // Both downloads that traded places are announced (the UI orders by both positions).
    let mut announced = Vec::new();
    let t = Instant::now();
    while t.elapsed() < Duration::from_millis(300) {
        if let Ok(Ok(ManagerEvent::Updated { download })) = tokio::time::timeout(Duration::from_millis(50), moved.recv()).await {
            announced.push(download.id);
        }
    }
    assert!(announced.contains(&a.id) && announced.contains(&b.id), "{announced:?}");
    let pos = |d: &download_manager::DownloadDto| d.position;
    let (pa, pb, pc) = (
        pos(&r.m.get_download(a.id).await.unwrap()),
        pos(&r.m.get_download(b.id).await.unwrap()),
        pos(&r.m.get_download(c.id).await.unwrap()),
    );
    assert!(pc < pb && pb < pa, "{pc} {pb} {pa}");
    let mut events = r.m.subscribe();
    r.m.cancel_download(blocker.id).await.unwrap();
    let mut order = Vec::new();
    let t = Instant::now();
    while order.len() < 3 && t.elapsed() < Duration::from_secs(20) {
        if let Ok(Ok(ManagerEvent::Updated { download })) = tokio::time::timeout(Duration::from_millis(200), events.recv()).await {
            // One slot: they finish in the order they started.
            if download.status == DownloadStatus::Completed && !order.contains(&download.id) {
                order.push(download.id);
            }
        }
    }
    assert_eq!(order, vec![c.id, b.id, a.id]);
}

#[tokio::test]
async fn an_urgent_download_does_not_wait_behind_a_big_low_priority_one() {
    let srv = serve(random_data(4 * MB, 9)).await;
    let (r, _e) = rig("express", 1).await;
    let big = r.m.create_download(NewDownload { priority: Some(Priority::Low), ..slow(&srv, "/big.bin", 256 * 1024) }).await.unwrap();
    wait_status(&r.m, big.id, DownloadStatus::Downloading).await;
    let normal = r.m.create_download(slow(&srv, "/normal.bin", 8 * MB as u64)).await.unwrap();
    stays(&r.m, normal.id, DownloadStatus::Queued, 200).await;
    let urgent =
        r.m.create_download(NewDownload { priority: Some(Priority::High), ..slow(&srv, "/urgent.bin", 8 * MB as u64) }).await.unwrap();
    wait_status(&r.m, urgent.id, DownloadStatus::Completed).await;
    assert_eq!(status(&r.m, normal.id).await, DownloadStatus::Queued, "the extra slot is only for urgent downloads");
    assert_eq!(status(&r.m, big.id).await, DownloadStatus::Downloading, "nothing was paused for it");
    r.m.cancel_download(big.id).await.unwrap();
    wait_status(&r.m, normal.id, DownloadStatus::Completed).await;
}

#[tokio::test]
async fn while_smart_throttle_slows_downloads_low_priority_ones_wait_and_running_ones_are_left_alone() {
    let srv = serve(random_data(4 * MB, 13)).await;
    let (r, _e) = rig("giving-way", 3).await;
    let call = vec!["VeloxTestCall".to_string()];
    r.m.set_smart_throttle(SetSmartThrottle { enabled: Some(true), auto: Some(false), apps: Some(call.clone()), ..Default::default() })
        .await
        .unwrap();
    let running =
        r.m.create_download(NewDownload { priority: Some(Priority::Low), ..slow(&srv, "/running.bin", 256 * 1024) }).await.unwrap();
    wait_status(&r.m, running.id, DownloadStatus::Downloading).await;
    // A call starts: downloads are slowed.
    r.m.smart_throttle_observe_signals(Some(&call), ThrottleSignals::default());
    assert!(r.m.smart_throttle().limit_bps.is_some());
    until("giving way", 10, || async { r.m.get_queues().await.unwrap().giving_way }).await;
    let low = r.m.create_download(NewDownload { priority: Some(Priority::Low), ..slow(&srv, "/low.bin", 8 * MB as u64) }).await.unwrap();
    let normal = r.m.create_download(slow(&srv, "/normal.bin", 8 * MB as u64)).await.unwrap();
    wait_status(&r.m, normal.id, DownloadStatus::Completed).await;
    assert_eq!(status(&r.m, low.id).await, DownloadStatus::Queued, "low priority waits for the line to be free");
    assert_eq!(status(&r.m, running.id).await, DownloadStatus::Downloading, "nothing running is stopped");
    // The call ends: it starts.
    r.m.smart_throttle_observe_signals(Some(&[]), ThrottleSignals::default());
    wait_status(&r.m, low.id, DownloadStatus::Completed).await;
    r.m.cancel_download(running.id).await.unwrap();
}

#[tokio::test]
async fn big_files_move_to_the_queue_made_for_them_unless_the_user_chose_a_queue() {
    let srv = serve(random_data(4 * MB, 10)).await;
    let (r, _e) = rig("bigfiles", 3).await;
    let rules = QueueRules { large_files_over: Some(MB as u64), ..night() };
    let q = add_queue(&r.m, "Big at night", rules).await;
    let d = r.m.create_download(slow(&srv, "/big.bin", 128 * 1024)).await.unwrap();
    until("moved to the big-file queue", 15, || async { r.m.get_download(d.id).await.unwrap().queue_id == q }).await;
    wait_status(&r.m, d.id, DownloadStatus::Paused).await;
    // The user puts it back in the main queue: it goes on there, and is never moved again.
    r.m.place_download(PlaceDownload { id: d.id, queue_id: Some(MAIN_QUEUE), start: None, step: None }).await.unwrap();
    wait_status(&r.m, d.id, DownloadStatus::Downloading).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(r.m.get_download(d.id).await.unwrap().queue_id, MAIN_QUEUE);
    r.m.cancel_download(d.id).await.unwrap();
}

#[tokio::test]
async fn queues_and_places_survive_a_restart_and_deleting_a_queue_keeps_its_downloads() {
    let srv = serve(random_data(MB, 11)).await;
    let e = env("restart");
    let clock = FakeClock::new(at(2, 12, 0));
    let sys = Arc::new(DryRunSystem::new());
    let (q, d) = {
        let m = rig_at(&e, 3, clock.clone(), sys.clone()).await;
        let q = add_queue(&m, "Night", night()).await;
        let d = m.create_download(NewDownload { queue_id: Some(q), start: Some(false), ..slow(&srv, "/k.bin", MB as u64) }).await.unwrap();
        m.queue_action(q, QueueAction::MoveUp).await.unwrap();
        m.shutdown().await;
        (q, d.id)
    };
    let m = rig_at(&e, 3, clock.clone(), sys.clone()).await;
    let qs: QueuesDto = m.get_queues().await.unwrap();
    assert_eq!(qs.queues.iter().map(|x| x.id).collect::<Vec<_>>(), vec![q, MAIN_QUEUE], "the order the user gave");
    assert_eq!(qs.queues[0].rules, night());
    assert_eq!(m.get_download(d).await.unwrap().queue_id, q);
    // The main queue cannot go; another one can, and its downloads move to the main queue.
    assert!(m.delete_queue(MAIN_QUEUE).await.is_err());
    m.delete_queue(q).await.unwrap();
    assert_eq!(m.get_download(d).await.unwrap().queue_id, MAIN_QUEUE);
    assert_eq!(m.get_queues().await.unwrap().queues.len(), 1);
}

#[tokio::test]
async fn clear_completed_removes_only_that_queues_finished_downloads() {
    let srv = serve(random_data(MB, 12)).await;
    let (r, _e) = rig("clear", 3).await;
    let q = add_queue(&r.m, "Mine", QueueRules::default()).await;
    let a = r.m.create_download(NewDownload { queue_id: Some(q), ..slow(&srv, "/a.bin", 8 * MB as u64) }).await.unwrap();
    let b = r.m.create_download(slow(&srv, "/b.bin", 8 * MB as u64)).await.unwrap();
    wait_status(&r.m, a.id, DownloadStatus::Completed).await;
    wait_status(&r.m, b.id, DownloadStatus::Completed).await;
    let file = r.m.get_download(a.id).await.unwrap().destination;
    r.m.queue_action(q, QueueAction::ClearCompleted).await.unwrap();
    assert!(r.m.get_download(a.id).await.is_err());
    assert!(std::path::Path::new(&file).exists(), "the file stays");
    assert_eq!(status(&r.m, b.id).await, DownloadStatus::Completed);
}

#[tokio::test]
async fn bad_requests_are_refused() {
    let (r, _e) = rig("bad", 3).await;
    let bad_rules =
        QueueRules { when: QueueWhen::Between { start: "25:00".into(), stop: "07:00".into(), days: [true; 7] }, ..Default::default() };
    assert!(r.m.save_queue(SaveQueue { id: None, name: "X".into(), rules: bad_rules }).await.is_err());
    assert!(r.m.save_queue(SaveQueue { id: None, name: "  ".into(), rules: QueueRules::default() }).await.is_err(), "a name is needed");
    assert!(r.m.save_queue(SaveQueue { id: None, name: "x".repeat(41), rules: QueueRules::default() }).await.is_err());
    assert!(r.m.save_queue(SaveQueue { id: Some(99), name: "X".into(), rules: QueueRules::default() }).await.is_err());
    assert!(r.m.queue_action(99, QueueAction::Start).await.is_err());
    for _ in 0..15 {
        let _ = r.m.save_queue(SaveQueue { id: None, name: "Q".into(), rules: QueueRules::default() }).await;
    }
    assert_eq!(r.m.get_queues().await.unwrap().queues.len(), 16, "at most 16 queues");
    // Requests from the UI are read strictly.
    assert!(serde_json::from_str::<SaveQueue>(r#"{"id":null,"name":"a","rules":{},"extra":1}"#).is_err());
    assert!(serde_json::from_str::<PlaceDownload>(r#"{"id":1,"start":{"kind":"at","at":"x","more":1}}"#).is_err());
    assert!(serde_json::from_str::<QueueAction>(r#"{"kind":"start"}"#).is_ok());
    assert!(serde_json::from_str::<QueueAction>(r#"{"kind":"shutdown"}"#).is_err());
    assert!(serde_json::from_str::<NewDownload>(r#"{"url":"https://a.b/c","queueId":2,"startAt":"2026-03-02T02:00"}"#).is_ok());
}
