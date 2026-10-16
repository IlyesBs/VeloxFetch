//! Game mode (feature #7): one click turns the gaming policy on now, whatever level is chosen and whatever runs, until
//! it ends (a time, or when the user turns it off). Through the real manager and engine against a local server; the
//! desktop app's status bar, banner, tray item and Ctrl+G only call `set_game_mode`.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use download_manager::profiles::ProfileAction;
use download_manager::{DownloadManager, GamePolicy, ManagerConfig, ManagerError, NewDownload, SetGameMode, SetSmartThrottle};
use downloader_core::DownloadStatus;
use velox_integration_tests::{random_data, serve};

const MB: usize = 1024 * 1024;
const KB: u64 = 1024;

fn base(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("velox-gm-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

fn db(b: &std::path::Path) -> PathBuf {
    b.join("state").join("velox.db")
}

async fn manager(b: &std::path::Path) -> DownloadManager {
    DownloadManager::new(ManagerConfig::new(db(b), b.join("downloads"))).await.unwrap()
}

async fn wait_for(what: &str, secs: u64, mut ok: impl AsyncFnMut() -> bool) {
    let t = Instant::now();
    while !ok().await {
        assert!(t.elapsed() < Duration::from_secs(secs), "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn wait_status(m: &DownloadManager, id: i64, want: DownloadStatus, secs: u64) {
    wait_for(&format!("{want:?}"), secs, async || m.get_download(id).await.unwrap().status == want).await;
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64
}

async fn on(m: &DownloadManager, minutes: Option<u32>) -> download_manager::SmartThrottleDto {
    m.set_game_mode(SetGameMode { on: Some(true), minutes, ..Default::default() }).await.unwrap()
}

async fn off(m: &DownloadManager) -> download_manager::SmartThrottleDto {
    m.set_game_mode(SetGameMode { on: Some(false), ..Default::default() }).await.unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn one_click_slows_downloads_with_smart_throttle_off_and_off_gives_full_speed_back() {
    let b = base("click");
    let m = manager(&b).await;
    let s = m.smart_throttle();
    assert!(!s.enabled && !s.game_mode.on && s.game_mode.until_ms.is_none() && s.game_mode.minutes == 120, "off by default: {s:?}");
    // Smart throttle stays off (Fast); the gaming policy is a low limit.
    m.set_smart_throttle(SetSmartThrottle { game_policy: Some(GamePolicy::Low), game_limit_bps: Some(256 * KB), ..Default::default() })
        .await
        .unwrap();
    let data = random_data(48 * MB, 71);
    let srv = serve(data.clone()).await;
    *srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(5);
    let d = m.create_download(NewDownload { url: srv.url("/big.bin"), ..Default::default() }).await.unwrap();
    wait_for("the download to run", 10, async || m.get_download(d.id).await.unwrap().downloaded > 0).await;

    // One click: on for the usual two hours, at once (no scan needed), with nothing listed running.
    let t = now_ms();
    let s = on(&m, None).await;
    assert!(s.game_mode.on && s.reason == "game" && s.limit_bps == Some(256 * KB), "{s:?}");
    let until = s.game_mode.until_ms.expect("ends in two hours");
    assert!((until - t - 120 * 60_000).abs() < 5_000, "two hours from now: {}", until - t);
    assert!(!s.enabled && s.active_app.is_none(), "the user's switch is not changed: {s:?}");
    // About 256 KB/s; the watcher's scans (nothing listed, Smart throttle off) keep it.
    let from = m.get_download(d.id).await.unwrap().downloaded;
    for _ in 0..8 {
        m.smart_throttle_observe(Some(&[]));
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let got = m.get_download(d.id).await.unwrap().downloaded - from;
    assert!(got > 0 && got < 1200 * KB, "held near 256 KB/s: {got} bytes in 2 s");
    assert_eq!(m.smart_throttle().reason, "game");

    // Off: full speed at once, the download finishes intact.
    let s = off(&m).await;
    assert!(!s.game_mode.on && s.limit_bps.is_none() && s.reason == "off", "{s:?}");
    *srv.state.chunk_delay.lock().unwrap() = Duration::ZERO;
    wait_status(&m, d.id, DownloadStatus::Completed, 60).await;
    assert_eq!(std::fs::read(m.get_download(d.id).await.unwrap().destination).unwrap(), *data);
    m.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn pause_policy_holds_the_queue_and_lets_go_of_only_what_it_held() {
    let b = base("pause");
    let m = manager(&b).await;
    m.set_smart_throttle(SetSmartThrottle { game_policy: Some(GamePolicy::Pause), ..Default::default() }).await.unwrap();
    let data = random_data(8 * MB, 72);
    let srv = serve(data.clone()).await;
    *srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(15);
    let a = m.create_download(NewDownload { url: srv.url("/a.bin"), ..Default::default() }).await.unwrap();
    let mine = m.create_download(NewDownload { url: srv.url("/b.bin"), ..Default::default() }).await.unwrap();
    wait_for("the downloads to run", 10, async || m.get_download(a.id).await.unwrap().downloaded > 0).await;
    m.pause_download(mine.id).await.unwrap();

    let s = on(&m, Some(0)).await;
    assert!(s.game_mode.on && s.game_mode.until_ms.is_none(), "until turned off: {s:?}");
    wait_for("the queue to hold", 20, async || {
        m.get_queue_state().await.unwrap().game_hold && m.get_download(a.id).await.unwrap().status == DownloadStatus::Paused
    })
    .await;
    let s = m.smart_throttle();
    assert!(s.game_hold && s.reason == "game", "{s:?}");
    let late = m.create_download(NewDownload { url: srv.url("/late.bin"), ..Default::default() }).await.unwrap();
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(m.get_download(late.id).await.unwrap().downloaded, 0, "nothing new starts in Game mode");

    off(&m).await;
    wait_for("the queue to let go", 10, async || !m.get_queue_state().await.unwrap().game_hold).await;
    wait_status(&m, a.id, DownloadStatus::Completed, 30).await;
    assert_eq!(std::fs::read(m.get_download(a.id).await.unwrap().destination).unwrap(), *data);
    wait_status(&m, late.id, DownloadStatus::Completed, 30).await;
    assert_eq!(m.get_download(mine.id).await.unwrap().status, DownloadStatus::Paused, "the user's own pause stays");
    m.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn game_mode_wins_over_every_level_and_profile_and_gives_each_back_when_it_ends() {
    let b = base("levels");
    let m = manager(&b).await;
    m.set_smart_throttle(SetSmartThrottle {
        enabled: Some(true),
        auto: Some(false),
        share_percent: Some(50),
        game_policy: Some(GamePolicy::Low),
        game_limit_bps: Some(128 * KB),
        apps: Some(vec!["VeloxTestGame".into()]),
        ..Default::default()
    })
    .await
    .unwrap();
    let manual = m.smart_throttle();
    assert_eq!(manual.reason, "manual");

    // Manual: Game mode's limit, then Manual's own again.
    let s = on(&m, None).await;
    assert!(s.reason == "game" && s.limit_bps == Some(128 * KB), "{s:?}");
    let s = off(&m).await;
    assert!(s.reason == "manual" && s.limit_bps == manual.limit_bps, "{s:?}");

    // Smart, with a listed program running: Game mode, then the program's own policy at the next scan.
    m.set_smart_throttle(SetSmartThrottle { auto: Some(true), ..Default::default() }).await.unwrap();
    on(&m, None).await;
    m.smart_throttle_observe(Some(&["VeloxTestGame".to_string()]));
    assert_eq!(m.smart_throttle().reason, "game", "Game mode is what is said while it is on");
    off(&m).await;
    m.smart_throttle_observe(Some(&["VeloxTestGame".to_string()]));
    let s = m.smart_throttle();
    assert!(s.reason == "app" && s.active_app.as_deref() == Some("VeloxTestGame") && s.limit_bps == Some(128 * KB), "{s:?}");
    m.smart_throttle_observe(Some(&[]));
    m.smart_throttle_observe(Some(&[]));

    // The Fast profile (never gives way, not even to a listed program): Game mode is the user's own click, it applies.
    m.profile_action(ProfileAction::Select { id: "fast".into() }).await.unwrap();
    assert!(!m.smart_throttle_enabled());
    let s = on(&m, None).await;
    assert!(s.reason == "game" && s.limit_bps == Some(128 * KB), "{s:?}");
    m.smart_throttle_observe(Some(&[]));
    assert_eq!(m.smart_throttle().limit_bps, Some(128 * KB), "the scans keep it");
    let s = off(&m).await;
    assert!(s.reason == "off" && s.limit_bps.is_none(), "Fast again: {s:?}");
    m.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn longer_usual_time_and_bad_requests() {
    let b = base("time");
    let m = manager(&b).await;
    // The usual time: one hour from now on.
    let s = m.set_game_mode(SetGameMode { default_minutes: Some(60), ..Default::default() }).await.unwrap();
    assert!(!s.game_mode.on && s.game_mode.minutes == 60, "{s:?}");
    let t = now_ms();
    let until = on(&m, None).await.game_mode.until_ms.unwrap();
    assert!((until - t - 60 * 60_000).abs() < 5_000);
    // One hour more.
    let later = m.set_game_mode(SetGameMode { add_minutes: Some(60), ..Default::default() }).await.unwrap().game_mode.until_ms.unwrap();
    assert_eq!(later - until, 60 * 60_000);
    // Never more than a day from now.
    for _ in 0..30 {
        m.set_game_mode(SetGameMode { add_minutes: Some(60), ..Default::default() }).await.unwrap();
    }
    let capped = m.smart_throttle().game_mode.until_ms.unwrap();
    assert!(capped - now_ms() <= 24 * 60 * 60_000 + 1_000, "at most a day: {}", capped - now_ms());
    // Until turned off: longer changes nothing.
    on(&m, Some(0)).await;
    let s = m.set_game_mode(SetGameMode { add_minutes: Some(60), ..Default::default() }).await.unwrap();
    assert!(s.game_mode.on && s.game_mode.until_ms.is_none(), "{s:?}");
    off(&m).await;
    // Longer while off: on for that long.
    let s = m.set_game_mode(SetGameMode { add_minutes: Some(30), ..Default::default() }).await.unwrap();
    assert!(s.game_mode.on && (s.game_mode.until_ms.unwrap() - now_ms() - 30 * 60_000).abs() < 5_000, "{s:?}");
    off(&m).await;

    for bad in [
        SetGameMode { on: Some(true), minutes: Some(24 * 60 + 1), ..Default::default() },
        SetGameMode { default_minutes: Some(100_000), ..Default::default() },
        SetGameMode { add_minutes: Some(0), ..Default::default() },
        SetGameMode { on: Some(false), add_minutes: Some(10), ..Default::default() },
    ] {
        let r = m.set_game_mode(bad.clone()).await;
        assert!(matches!(r, Err(ManagerError::InvalidInput(_))), "{bad:?}: {r:?}");
    }
    assert!(!m.smart_throttle().game_mode.on, "nothing turned on by a refused request");
    let unknown = serde_json::from_str::<SetGameMode>(r#"{"on":true,"forever":true}"#);
    assert!(unknown.is_err(), "unknown fields are refused");
    m.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn kept_across_a_restart_until_it_ends_and_an_ended_one_is_forgotten() {
    let b = base("restart");
    let data = random_data(6 * MB, 73);
    let srv = serve(data.clone()).await;
    *srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(15);
    let m = manager(&b).await;
    m.set_smart_throttle(SetSmartThrottle { game_policy: Some(GamePolicy::Pause), ..Default::default() }).await.unwrap();
    let a = m.create_download(NewDownload { url: srv.url("/a.bin"), ..Default::default() }).await.unwrap();
    wait_for("the download to run", 10, async || m.get_download(a.id).await.unwrap().downloaded > 0).await;
    on(&m, Some(0)).await;
    wait_status(&m, a.id, DownloadStatus::Paused, 20).await;
    m.shutdown().await;
    drop(m);

    // Launched again: still on, the queue held from the first moment; what it paused stays paused.
    let m = manager(&b).await;
    let s = m.smart_throttle();
    assert!(s.game_mode.on && s.reason == "game" && s.game_hold, "{s:?}");
    let q = m.get_queue_state().await.unwrap();
    assert!(q.game_hold && !q.gate_open && q.held == 1, "held before anything starts: {q:?}");
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(m.get_download(a.id).await.unwrap().status, DownloadStatus::Paused);
    m.shutdown().await;
    drop(m);

    // It ended while the app was closed: off at start, what it paused goes on by itself and finishes intact.
    *srv.state.chunk_delay.lock().unwrap() = Duration::ZERO;
    let store = persistence::SqliteStore::open(db(&b)).unwrap();
    store.set_setting("game_mode.until", &(now_ms() - 1_000).to_string()).unwrap();
    drop(store);
    let m = manager(&b).await;
    let s = m.smart_throttle();
    assert!(!s.game_mode.on && !s.game_hold && s.limit_bps.is_none(), "{s:?}");
    wait_status(&m, a.id, DownloadStatus::Completed, 30).await;
    assert_eq!(std::fs::read(m.get_download(a.id).await.unwrap().destination).unwrap(), *data);
    m.shutdown().await;
    drop(m);

    // It ends while running: the next scan lets go, and it is forgotten for the next start.
    let store = persistence::SqliteStore::open(db(&b)).unwrap();
    store.set_setting("game_mode.until", &(now_ms() + 1_500).to_string()).unwrap();
    drop(store);
    let m = manager(&b).await;
    assert!(m.smart_throttle().game_mode.on && m.get_queue_state().await.unwrap().game_hold);
    tokio::time::sleep(Duration::from_millis(1_700)).await;
    assert!(!m.smart_throttle().game_mode.on, "ended on time, even before a scan");
    m.smart_throttle_observe(Some(&[]));
    wait_for("the queue to let go", 10, async || !m.get_queue_state().await.unwrap().game_hold).await;
    let s = m.smart_throttle();
    assert!(s.reason == "off" && s.limit_bps.is_none(), "{s:?}");
    m.shutdown().await;
    drop(m);
    let store = persistence::SqliteStore::open(db(&b)).unwrap();
    assert_eq!(store.get_setting("game_mode.until").unwrap(), None, "forgotten");
    // A broken value is read as off.
    store.set_setting("game_mode.until", "soon").unwrap();
    drop(store);
    let m = manager(&b).await;
    assert!(!m.smart_throttle().game_mode.on);
    m.shutdown().await;
}
