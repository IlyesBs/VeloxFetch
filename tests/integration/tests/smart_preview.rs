//! Smart throttle and live preview, through the real manager and engine against a local server.
//! (The desktop app's process scan and its custom protocol are thin layers over what is tested here.)

use std::path::PathBuf;
use std::time::{Duration, Instant};

use download_manager::{
    DownloadManager, GamePolicy, ManagerConfig, ManagerError, NewDownload, PauseQueue, SetSmartThrottle, SetSpeedLimit, SpeedTarget,
    ThrottleSignals,
};
use downloader_core::DownloadStatus;
use velox_integration_tests::{random_data, serve};

const MB: usize = 1024 * 1024;

fn base(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("velox-sp-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

async fn manager(base: &std::path::Path) -> DownloadManager {
    DownloadManager::new(ManagerConfig::new(base.join("state").join("velox.db"), base.join("downloads"))).await.unwrap()
}

async fn wait_status(m: &DownloadManager, id: i64, want: DownloadStatus, secs: u64) {
    let t = Instant::now();
    loop {
        let d = m.get_download(id).await.unwrap();
        if d.status == want {
            return;
        }
        assert!(t.elapsed() < Duration::from_secs(secs), "still {:?} ({} bytes)", d.status, d.downloaded);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn names(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

// ------------------------------------------------------------------------------------ smart throttle

#[tokio::test(flavor = "multi_thread")]
async fn smart_throttle_slows_downloads_while_a_listed_program_runs_and_lifts_when_it_closes() {
    let b = base("throttle");
    let m = manager(&b).await;
    let s = m.smart_throttle();
    assert!(!s.enabled && s.active_app.is_none() && s.limit_bps.is_none(), "off by default: {s:?}");
    assert!(s.apps.iter().any(|a| a == "League of Legends") && s.apps.iter().any(|a| a == "Zoom"));

    // Off: a listed program changes nothing.
    m.smart_throttle_observe(Some(&names(&["Zoom.exe"])));
    assert!(m.smart_throttle().limit_bps.is_none());

    m.set_smart_throttle(SetSmartThrottle {
        enabled: Some(true),
        apps: Some(names(&["VeloxTestGame"])),
        share_percent: Some(25),
        ..Default::default()
    })
    .await
    .unwrap();
    m.smart_throttle_observe(Some(&names(&["explorer.exe", "VELOXTESTGAME.EXE"])));
    let s = m.smart_throttle();
    assert_eq!(s.active_app.as_deref(), Some("VeloxTestGame"));
    assert_eq!(s.limit_bps, Some(1024 * 1024), "before any speed is known: 1 MB/s");
    assert_eq!(s.game_policy, GamePolicy::Adaptive, "the default policy is the one it always had");

    // A download started while the game runs is held to about 1 MB/s.
    let data = random_data(6 * MB, 7);
    let srv = serve(data.clone()).await;
    let d = m.create_download(NewDownload { url: srv.url("/big.bin"), ..Default::default() }).await.unwrap();
    tokio::time::sleep(Duration::from_millis(2000)).await;
    let mid = m.get_download(d.id).await.unwrap().downloaded;
    assert!(mid < 4 * MB as u64, "throttled: {mid} bytes after 2 s");
    assert!(mid > 0, "but still moving");

    // The game closes: full speed after two scans without it (one could be a restart), not at the next restart of
    // the download.
    m.smart_throttle_observe(Some(&names(&["explorer.exe"])));
    assert!(m.smart_throttle().limit_bps.is_some(), "one scan without it: kept");
    m.smart_throttle_observe(Some(&names(&["explorer.exe"])));
    assert!(m.smart_throttle().limit_bps.is_none());
    let t = Instant::now();
    wait_status(&m, d.id, DownloadStatus::Completed, 10).await;
    assert!(t.elapsed() < Duration::from_secs(3), "the rest went at full speed: {:?}", t.elapsed());
    assert_eq!(std::fs::read(m.get_download(d.id).await.unwrap().destination).unwrap(), *data);

    // A failed scan keeps the policy for a short while (no burst over the game), and a scan that works decides.
    m.smart_throttle_observe(Some(&names(&["VeloxTestGame.exe"])));
    assert!(m.smart_throttle().limit_bps.is_some());
    m.smart_throttle_observe(None);
    let s = m.smart_throttle();
    assert!(s.limit_bps.is_some() && !s.scan_ok && s.active_app.is_some(), "{s:?}");
    m.smart_throttle_observe(Some(&[]));
    m.smart_throttle_observe(Some(&[]));
    let s = m.smart_throttle();
    assert!(s.limit_bps.is_none() && s.scan_ok, "{s:?}");
    // Without a game, a failed scan slows nothing.
    m.smart_throttle_observe(None);
    assert!(m.smart_throttle().limit_bps.is_none());

    // Turning it off lifts the cap immediately, without waiting for a scan.
    m.smart_throttle_observe(Some(&names(&["VeloxTestGame.exe"])));
    m.set_smart_throttle(SetSmartThrottle { enabled: Some(false), ..Default::default() }).await.unwrap();
    assert!(m.smart_throttle().limit_bps.is_none());
    m.shutdown().await;

    // The settings survive a restart; the cap does not (nothing is running yet).
    let m = manager(&b).await;
    let s = m.smart_throttle();
    assert!(!s.enabled && s.apps == names(&["VeloxTestGame"]) && s.share_percent == 25 && s.limit_bps.is_none(), "{s:?}");
    // A full path is a valid entry (one program among several with the same file name).
    assert!(m
        .set_smart_throttle(SetSmartThrottle { apps: Some(names(&["C:\\Games\\x.exe", "VeloxTestGame"])), ..Default::default() })
        .await
        .is_ok());
    m.set_smart_throttle(SetSmartThrottle { apps: Some(names(&["VeloxTestGame"])), ..Default::default() }).await.unwrap();
    for bad in [
        SetSmartThrottle { share_percent: Some(0), ..Default::default() },
        SetSmartThrottle { share_percent: Some(95), ..Default::default() },
        SetSmartThrottle { apps: Some(names(&["C:\\Games\\*.exe"])), ..Default::default() },
        SetSmartThrottle { apps: Some(names(&["Games\\x.exe"])), ..Default::default() },
        SetSmartThrottle { min_bps: Some(1024), ..Default::default() },
        SetSmartThrottle { min_bps: Some(2 * 1024 * 1024 * 1024), ..Default::default() },
    ] {
        assert!(matches!(m.set_smart_throttle(bad).await, Err(ManagerError::InvalidInput(_))));
    }
}

fn browsing() -> ThrottleSignals {
    ThrottleSignals { other_bps: Some(400.0 * 1024.0), idle: Some(Duration::ZERO), ..Default::default() }
}

fn quiet(idle_secs: u64) -> ThrottleSignals {
    ThrottleSignals { other_bps: Some(0.0), idle: Some(Duration::from_secs(idle_secs)), ..Default::default() }
}

#[tokio::test(flavor = "multi_thread")]
async fn automatic_mode_gives_way_to_browsing_and_takes_the_line_back_when_it_is_free() {
    let b = base("auto");
    let m = manager(&b).await;
    let s = m.smart_throttle();
    assert!(s.auto, "automatic mode is on by default (once Smart throttle itself is on)");
    assert_eq!(s.min_bps, 256 * 1024);
    assert_eq!(s.reason, "off");

    // Off: browsing changes nothing.
    for _ in 0..3 {
        m.smart_throttle_observe_signals(Some(&[]), browsing());
    }
    assert!(m.smart_throttle().limit_bps.is_none());

    m.set_smart_throttle(SetSmartThrottle { enabled: Some(true), apps: Some(vec![]), ..Default::default() }).await.unwrap();
    m.smart_throttle_observe_signals(Some(&[]), quiet(0));
    assert_eq!(m.smart_throttle().reason, "fullFree", "downloads alone: full speed");
    m.smart_throttle_observe_signals(Some(&[]), quiet(600));
    assert_eq!(m.smart_throttle().reason, "fullIdle");

    // Browsing: after the confirmation, downloads slow down (here to the default 1 MB/s: no speed learned yet).
    let data = random_data(8 * MB, 9);
    let srv = serve(data.clone()).await;
    for _ in 0..3 {
        m.smart_throttle_observe_signals(Some(&[]), browsing());
    }
    let s = m.smart_throttle();
    assert_eq!((s.reason.as_str(), s.limit_bps), ("browsing", Some(1024 * 1024)), "{s:?}");
    let d = m.create_download(NewDownload { url: srv.url("/auto.bin"), ..Default::default() }).await.unwrap();
    tokio::time::sleep(Duration::from_millis(2000)).await;
    let mid = m.get_download(d.id).await.unwrap().downloaded;
    assert!(mid > 0 && mid < 5 * MB as u64, "slowed but moving: {mid} bytes after 2 s");

    // The network calms down: held for a while (easing), then back to full speed, and the download finishes.
    let mut reasons = Vec::new();
    for _ in 0..40 {
        m.smart_throttle_observe_signals(Some(&[]), quiet(0));
        let r = m.smart_throttle().reason;
        if reasons.last() != Some(&r) {
            reasons.push(r.clone());
        }
        if r == "fullFree" {
            break;
        }
    }
    assert_eq!(reasons.last().map(String::as_str), Some("fullFree"), "{reasons:?}");
    assert!(reasons.iter().any(|r| r == "easing"), "it eased back up: {reasons:?}");
    assert!(m.smart_throttle().limit_bps.is_none());
    wait_status(&m, d.id, DownloadStatus::Completed, 15).await;
    assert_eq!(std::fs::read(m.get_download(d.id).await.unwrap().destination).unwrap(), *data);

    // Automatic mode off: browsing is ignored again (listed programs still count).
    m.set_smart_throttle(SetSmartThrottle { auto: Some(false), ..Default::default() }).await.unwrap();
    for _ in 0..3 {
        m.smart_throttle_observe_signals(Some(&[]), browsing());
    }
    assert!(m.smart_throttle().limit_bps.is_none());

    // A higher reserved minimum is kept, and survives a restart.
    m.set_smart_throttle(SetSmartThrottle { auto: Some(true), min_bps: Some(3 * 1024 * 1024), ..Default::default() }).await.unwrap();
    for _ in 0..3 {
        m.smart_throttle_observe_signals(Some(&[]), browsing());
    }
    assert_eq!(m.smart_throttle().limit_bps, Some(3 * 1024 * 1024));
    m.shutdown().await;
    let m = manager(&b).await;
    let s = m.smart_throttle();
    assert!(s.enabled && s.auto && s.min_bps == 3 * 1024 * 1024 && s.limit_bps.is_none(), "{s:?}");
    m.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_cap_follows_the_speed_this_connection_can_reach() {
    let b = base("learn");
    let m = manager(&b).await;
    m.set_smart_throttle(SetSmartThrottle {
        enabled: Some(true),
        apps: Some(names(&["VeloxTestCall"])),
        share_percent: Some(50),
        ..Default::default()
    })
    .await
    .unwrap();
    let srv = serve(random_data(40 * MB, 3)).await;
    *srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(2);
    let d = m.create_download(NewDownload { url: srv.url("/learn.bin"), ..Default::default() }).await.unwrap();
    // The watcher reports every few seconds; here every 200 ms while the download runs at full speed.
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(3) {
        m.smart_throttle_observe(Some(&[]));
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    m.smart_throttle_observe(Some(&names(&["VeloxTestCall"])));
    let limit = m.smart_throttle().limit_bps.unwrap();
    assert!(limit > 1024 * 1024, "half of a fast local transfer is more than the default: {limit}");
    m.smart_throttle_observe(Some(&[]));
    // A fast machine may have finished the 40 MB already: only a running download needs cancelling.
    match m.cancel_download(d.id).await {
        Ok(_) | Err(ManagerError::InvalidState { .. }) => {}
        Err(e) => panic!("{e}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn changing_modes_while_capped_never_stops_a_running_download() {
    // A download slowed for "a game" while the user flips through the modes (and the cap goes down and up): its
    // bytes keep coming every half second, never a pause and then a burst.
    let b = base("modes");
    let m = manager(&b).await;
    m.set_smart_throttle(SetSmartThrottle {
        enabled: Some(true),
        auto: Some(false),
        apps: Some(names(&["VeloxTestGame"])),
        share_percent: Some(25),
        ..Default::default()
    })
    .await
    .unwrap();
    let srv = serve(random_data(96 * MB, 21)).await;
    *srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(20);
    let d = m.create_download(NewDownload { url: srv.url("/modes.bin"), ..Default::default() }).await.unwrap();
    // Free first: what downloads get is learned (the watcher looks every few seconds; here more often).
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(2) {
        m.smart_throttle_observe(Some(&[]));
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    m.smart_throttle_observe(Some(&names(&["VeloxTestGame"])));
    assert!(m.smart_throttle().limit_bps.is_some());
    let mut samples = vec![(Instant::now(), m.get_download(d.id).await.unwrap().downloaded)];
    for share in [50u8, 10, 25, 50, 25, 10, 50] {
        m.set_smart_throttle(SetSmartThrottle { share_percent: Some(share), ..Default::default() }).await.unwrap();
        let t = Instant::now();
        while t.elapsed() < Duration::from_millis(1500) {
            tokio::time::sleep(Duration::from_millis(100)).await;
            m.smart_throttle_observe(Some(&names(&["VeloxTestGame"])));
            samples.push((Instant::now(), m.get_download(d.id).await.unwrap().downloaded));
        }
    }
    let status = m.get_download(d.id).await.unwrap().status;
    assert_eq!(status, DownloadStatus::Downloading, "still running at the end (else the check below says nothing)");
    // Every 500 ms window has bytes.
    let mut still = Vec::new();
    for (i, (at, bytes)) in samples.iter().enumerate() {
        if let Some((_, later)) = samples[i..].iter().find(|(t, _)| t.duration_since(*at) >= Duration::from_millis(500)) {
            if later == bytes {
                still.push(at.duration_since(samples[0].0).as_millis());
            }
        }
    }
    assert!(still.is_empty(), "no bytes for half a second at (ms): {still:?}");
    let _ = m.cancel_download(d.id).await;
}

// ------------------------------------------------------------------------------------ gaming policy

async fn wait_for(what: &str, secs: u64, mut ok: impl AsyncFnMut() -> bool) {
    let t = Instant::now();
    while !ok().await {
        assert!(t.elapsed() < Duration::from_secs(secs), "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn gaming(m: &DownloadManager, policy: GamePolicy) {
    m.set_smart_throttle(SetSmartThrottle {
        enabled: Some(true),
        auto: Some(false),
        apps: Some(names(&["VeloxTestGame", "OtherTestGame"])),
        game_policy: Some(policy),
        game_limit_bps: Some(256 * 1024),
        ..Default::default()
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn pause_policy_pauses_downloads_during_a_game_and_resumes_only_those_it_paused() {
    let b = base("game-pause");
    let m = manager(&b).await;
    gaming(&m, GamePolicy::Pause).await;
    let data = random_data(8 * MB, 31);
    let srv = serve(data.clone()).await;
    *srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(15);
    let a = m.create_download(NewDownload { url: srv.url("/a.bin"), ..Default::default() }).await.unwrap();
    let mine = m.create_download(NewDownload { url: srv.url("/b.bin"), ..Default::default() }).await.unwrap();
    let before = m.create_download(NewDownload { url: srv.url("/c.bin"), ..Default::default() }).await.unwrap();
    wait_for("the downloads to run", 10, async || m.get_download(a.id).await.unwrap().downloaded > 0).await;
    // Paused by the user before the game.
    m.pause_download(before.id).await.unwrap();

    // The game starts: running downloads pause (progress kept), nothing new starts.
    m.smart_throttle_observe(Some(&names(&["VeloxTestGame.exe"])));
    wait_for("the queue to hold", 20, async || {
        m.get_queue_state().await.unwrap().game_hold
            && m.get_download(a.id).await.unwrap().status == DownloadStatus::Paused
            && m.get_download(mine.id).await.unwrap().status == DownloadStatus::Paused
    })
    .await;
    let q = m.get_queue_state().await.unwrap();
    assert!(!q.gate_open && !q.paused && q.held == 2, "held for the game, not paused by the user: {q:?}");
    let s = m.smart_throttle();
    assert!(s.game_hold && s.reason == "app" && s.limit_bps == Some(s.min_bps), "{s:?}");
    let kept = m.get_download(a.id).await.unwrap().downloaded;
    assert!(kept > 0, "progress kept");
    let late = m.create_download(NewDownload { url: srv.url("/late.bin"), ..Default::default() }).await.unwrap();
    // The user pauses one of the held downloads during the game: it is theirs now.
    m.pause_download(mine.id).await.unwrap();

    // Another listed game, then the first closes: still held (and the same scan again changes nothing).
    for _ in 0..3 {
        m.smart_throttle_observe(Some(&names(&["OtherTestGame"])));
    }
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(m.get_queue_state().await.unwrap().game_hold);
    assert_eq!(m.get_download(late.id).await.unwrap().downloaded, 0, "nothing new started during the game");
    assert_eq!(m.get_download(a.id).await.unwrap().status, DownloadStatus::Paused);

    // All games closed (two scans): what the game paused resumes and finishes intact; the user's pauses stay.
    m.smart_throttle_observe(Some(&[]));
    m.smart_throttle_observe(Some(&[]));
    wait_for("the queue to let go", 10, async || !m.get_queue_state().await.unwrap().game_hold).await;
    wait_status(&m, a.id, DownloadStatus::Completed, 30).await;
    assert_eq!(std::fs::read(m.get_download(a.id).await.unwrap().destination).unwrap(), *data);
    wait_status(&m, late.id, DownloadStatus::Completed, 30).await;
    assert_eq!(m.get_download(mine.id).await.unwrap().status, DownloadStatus::Paused, "paused by the user during the game");
    assert_eq!(m.get_download(before.id).await.unwrap().status, DownloadStatus::Paused, "paused by the user before the game");
    let q = m.get_queue_state().await.unwrap();
    assert!(q.gate_open && q.held == 0 && m.smart_throttle().limit_bps.is_none(), "{q:?}");
    m.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_users_own_queue_pause_and_limits_outlast_a_game() {
    let b = base("game-user");
    let m = manager(&b).await;
    gaming(&m, GamePolicy::Pause).await;
    m.set_speed_limit(SetSpeedLimit { target: SpeedTarget::Global, bytes_per_sec: Some(2 * MB as u64) }).await.unwrap();
    // The user paused the queue (running downloads go on) before the game.
    let srv = serve(random_data(24 * MB, 33)).await;
    *srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(15);
    let a = m.create_download(NewDownload { url: srv.url("/a.bin"), ..Default::default() }).await.unwrap();
    wait_for("the download to run", 10, async || m.get_download(a.id).await.unwrap().downloaded > 0).await;
    m.pause_queue(PauseQueue { pause_running: false }).await.unwrap();
    assert_eq!(m.get_download(a.id).await.unwrap().status, DownloadStatus::Downloading);

    // The game pauses the running download although the gate was already closed.
    m.smart_throttle_observe(Some(&names(&["VeloxTestGame"])));
    wait_status(&m, a.id, DownloadStatus::Paused, 20).await;
    // During the game the user changes their own limit.
    m.set_speed_limit(SetSpeedLimit { target: SpeedTarget::Global, bytes_per_sec: Some(3 * MB as u64) }).await.unwrap();

    // The game closes: the user's queue pause and their new limit stay; nothing resumes while the queue is paused.
    m.smart_throttle_observe(Some(&[]));
    m.smart_throttle_observe(Some(&[]));
    wait_for("the game hold to lift", 10, async || !m.get_queue_state().await.unwrap().game_hold).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let q = m.get_queue_state().await.unwrap();
    assert!(q.paused && !q.gate_open && q.held == 1, "{q:?}");
    assert_eq!(m.get_download(a.id).await.unwrap().status, DownloadStatus::Paused);
    assert_eq!(m.speed_limits().global, Some(3 * MB as u64), "the user's change during the game is kept");
    // The user resumes the queue: the held download goes on.
    m.resume_queue().await.unwrap();
    wait_for("the download to resume", 10, async || m.get_download(a.id).await.unwrap().status == DownloadStatus::Downloading).await;
    let _ = m.cancel_download(a.id).await;
    m.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restart_during_a_game_never_leaves_downloads_held() {
    let b = base("game-restart");
    let m = manager(&b).await;
    gaming(&m, GamePolicy::Pause).await;
    let data = random_data(6 * MB, 35);
    let srv = serve(data.clone()).await;
    *srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(15);
    let a = m.create_download(NewDownload { url: srv.url("/a.bin"), ..Default::default() }).await.unwrap();
    wait_for("the download to run", 10, async || m.get_download(a.id).await.unwrap().downloaded > 0).await;
    m.smart_throttle_observe(Some(&names(&["VeloxTestGame"])));
    wait_status(&m, a.id, DownloadStatus::Paused, 20).await;
    m.shutdown().await;
    drop(m);

    // Launched again with no scan yet (or the game gone): nothing is held, the download goes on by itself.
    *srv.state.chunk_delay.lock().unwrap() = Duration::ZERO;
    let m = manager(&b).await;
    let s = m.smart_throttle();
    assert!(s.enabled && s.game_policy == GamePolicy::Pause && s.game_limit_bps == 256 * 1024, "settings kept: {s:?}");
    assert!(!s.game_hold && s.limit_bps.is_none() && s.active_app.is_none(), "no stale override: {s:?}");
    wait_status(&m, a.id, DownloadStatus::Completed, 30).await;
    assert_eq!(std::fs::read(m.get_download(a.id).await.unwrap().destination).unwrap(), *data);
    assert!(!m.get_queue_state().await.unwrap().game_hold);
    m.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn changing_the_policy_during_a_game_applies_at_once_and_flickers_change_nothing() {
    let b = base("game-switch");
    let m = manager(&b).await;
    gaming(&m, GamePolicy::Low).await;
    let srv = serve(random_data(64 * MB, 37)).await;
    *srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(5);
    let a = m.create_download(NewDownload { url: srv.url("/a.bin"), ..Default::default() }).await.unwrap();
    wait_for("the download to run", 10, async || m.get_download(a.id).await.unwrap().downloaded > 0).await;

    m.smart_throttle_observe(Some(&names(&["VeloxTestGame"])));
    assert_eq!(m.smart_throttle().limit_bps, Some(256 * 1024), "low: the chosen limit");
    // About 256 KB/s while the game runs.
    let from = m.get_download(a.id).await.unwrap().downloaded;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let got = m.get_download(a.id).await.unwrap().downloaded - from;
    assert!(got > 0 && got < 1200 * 1024, "held near 256 KB/s: {got} bytes in 2 s");

    // A game that flickers (seen, missed once, seen...) never lifts the limit.
    let game = names(&["VeloxTestGame"]);
    for i in 0..10 {
        m.smart_throttle_observe(Some(if i % 2 == 0 { &[] } else { &game[..] }));
        assert_eq!(m.smart_throttle().limit_bps, Some(256 * 1024), "scan {i}");
    }

    // The user switches to pause during the game: held at once; back to adaptive: let go at once.
    m.set_smart_throttle(SetSmartThrottle { game_policy: Some(GamePolicy::Pause), ..Default::default() }).await.unwrap();
    wait_status(&m, a.id, DownloadStatus::Paused, 20).await;
    assert!(m.smart_throttle().game_hold);
    m.set_smart_throttle(SetSmartThrottle { game_policy: Some(GamePolicy::Adaptive), ..Default::default() }).await.unwrap();
    wait_for("the download to resume", 10, async || m.get_download(a.id).await.unwrap().status == DownloadStatus::Downloading).await;
    let s = m.smart_throttle();
    assert!(!s.game_hold && s.limit_bps.is_some() && s.reason == "app", "adaptive: the mode's share while the game runs: {s:?}");
    // Turned off during the game: full speed and nothing held.
    m.set_smart_throttle(SetSmartThrottle { enabled: Some(false), ..Default::default() }).await.unwrap();
    let s = m.smart_throttle();
    assert!(s.limit_bps.is_none() && !s.game_hold && s.active_app.is_none(), "{s:?}");
    for bad in [Some(1024), Some(2 * 1024 * 1024 * 1024)] {
        let r = m.set_smart_throttle(SetSmartThrottle { game_limit_bps: bad, ..Default::default() }).await;
        assert!(matches!(r, Err(ManagerError::InvalidInput(_))), "{bad:?}");
    }
    let _ = m.cancel_download(a.id).await;
    m.shutdown().await;
}

// -------------------------------------------------------------------------------------- live preview

#[tokio::test(flavor = "multi_thread")]
async fn a_video_can_be_watched_while_it_downloads_and_unwritten_bytes_are_never_served() {
    let b = base("preview");
    let m = manager(&b).await;
    let data = random_data(12 * MB, 11);
    let srv = serve(data.clone()).await;
    *srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(60);
    let d = m.create_download(NewDownload { url: srv.url("/movie.mp4"), start: Some(false), ..Default::default() }).await.unwrap();

    // About 1 MB/s (12 s for the file), whatever the machine's load: the order is what is checked.
    m.set_speed_limit(download_manager::SetSpeedLimit { target: download_manager::SpeedTarget::Global, bytes_per_sec: Some(MB as u64) })
        .await
        .unwrap();
    // Preview order is asked for before it starts: the start and the end come first.
    let info = m.set_preview_mode(d.id, true).await.unwrap();
    assert!(info.preview_mode && info.total == 0, "the size is not known before the server was asked: {info:?}");
    m.start_download(d.id).await.unwrap();
    let t = Instant::now();
    let tail = loop {
        let r = m.read_preview(d.id, Some("bytes=-4096")).await;
        if r.status == 206 {
            break r;
        }
        assert!(t.elapsed() < Duration::from_secs(25), "the end never became available ({})", r.status);
    };
    let ready = m.preview_info(d.id).await.unwrap().ready_from_start;
    // The end is there long before the file is (a loaded machine may have fetched up to about half by then).
    assert!(ready <= data.len() as u64 * 3 / 4, "the end came before most of the file: {ready} bytes from the start");
    m.set_speed_limit(download_manager::SetSpeedLimit { target: download_manager::SpeedTarget::Global, bytes_per_sec: None })
        .await
        .unwrap();
    let total = data.len();
    assert_eq!(tail.content_range.as_deref(), Some(format!("bytes {}-{}/{total}", total - 4096, total - 1).as_str()));
    assert_eq!(tail.body, data[total - 4096..], "the end of the file, exactly");
    assert_eq!(tail.content_type, "video/mp4");
    let info = m.preview_info(d.id).await.unwrap();
    assert!(info.preview_mode && !info.complete && info.total == total as u64, "{info:?}");
    assert_ne!(m.get_download(d.id).await.unwrap().status, DownloadStatus::Completed, "the end arrived before the whole file");

    // Reading from the start: every answer is exactly the file's bytes, never zeros from the unwritten part.
    let mut at = 0usize;
    let t = Instant::now();
    while at < total {
        let r = m.read_preview(d.id, Some(&format!("bytes={at}-"))).await;
        assert!(r.status == 206 || r.status == 503, "{}", r.status);
        if r.status == 206 {
            assert!(r.body.len() as u64 <= 2 * MB as u64, "one chunk at most");
            assert_eq!(r.body, data[at..at + r.body.len()], "bytes {at}..");
            assert_eq!(r.content_range.as_deref(), Some(format!("bytes {at}-{}/{total}", at + r.body.len() - 1).as_str()));
            at += r.body.len();
        }
        assert!(t.elapsed() < Duration::from_secs(60), "stuck at {at}");
    }
    wait_status(&m, d.id, DownloadStatus::Completed, 30).await;
    assert_eq!(std::fs::read(m.get_download(d.id).await.unwrap().destination).unwrap(), *data, "preview did not disturb the download");

    // Finished: the saved file is served; out-of-range and odd requests get proper answers.
    let r = m.read_preview(d.id, Some("bytes=100-199")).await;
    assert_eq!((r.status, r.body.as_slice()), (206, &data[100..200]));
    let r = m.read_preview(d.id, None).await;
    assert_eq!((r.status, r.body.len()), (206, 2 * MB), "no Range: the start, one chunk");
    let r = m.read_preview(d.id, Some(&format!("bytes={total}-"))).await;
    assert_eq!((r.status, r.content_range), (416, Some(format!("bytes */{total}"))));
    assert!(m.preview_info(d.id).await.unwrap().complete);
}

#[tokio::test]
async fn only_audio_and_video_downloads_can_be_previewed() {
    let b = base("refuse");
    let m = manager(&b).await;
    let srv = serve(random_data(MB, 2)).await;
    let page = m.create_download(NewDownload { url: srv.url("/page.html"), ..Default::default() }).await.unwrap();
    let prog = m.create_download(NewDownload { url: srv.url("/setup.exe"), ..Default::default() }).await.unwrap();
    for id in [page.id, prog.id] {
        wait_status(&m, id, DownloadStatus::Completed, 20).await;
        assert_eq!(m.read_preview(id, Some("bytes=0-")).await.status, 415, "a web page or a program is never served");
        assert!(m.set_preview_mode(id, true).await.is_err());
    }
    assert_eq!(m.read_preview(999_999, None).await.status, 404);
}

#[tokio::test(flavor = "multi_thread")]
async fn watching_a_download_already_running_gets_its_start_first() {
    let b = base("preview-running");
    let m = manager(&b).await;
    let data = random_data(5 * MB, 12);
    let srv = serve(data.clone()).await;
    // Slow (about 200 KB/s overall), as on a slow line: the order is what is checked.
    m.set_speed_limit(download_manager::SetSpeedLimit { target: download_manager::SpeedTarget::Global, bytes_per_sec: Some(200 * 1024) })
        .await
        .unwrap();
    let d = m.create_download(NewDownload { url: srv.url("/song.wav"), ..Default::default() }).await.unwrap();
    let t = Instant::now();
    while m.get_download(d.id).await.unwrap().downloaded < 300 * 1024 {
        assert!(t.elapsed() < Duration::from_secs(20), "never started");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // "Watch while downloading" pressed now.
    m.set_preview_mode(d.id, true).await.unwrap();
    let t = Instant::now();
    loop {
        let info = m.preview_info(d.id).await.unwrap();
        if info.ready_from_start > 0 {
            eprintln!(
                "start ready after {:?}: {} bytes ({} downloaded)",
                t.elapsed(),
                info.ready_from_start,
                m.get_download(d.id).await.unwrap().downloaded
            );
            break;
        }
        assert!(
            t.elapsed() < Duration::from_secs(8),
            "the start of the file is still not playable: {info:?}, {} downloaded",
            m.get_download(d.id).await.unwrap().downloaded
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let r = m.read_preview(d.id, Some("bytes=0-")).await;
    assert_eq!(r.status, 206);
    assert_eq!(r.body, data[..r.body.len()], "the start, exactly");
    // It keeps leading: the first unfinished part (where the player is) never sits idle while the rest downloads (a
    // part that has to queue again keeps its place; it never waits behind the middle of the file).
    let mut idle_since: Option<(usize, u64, Instant)> = None;
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(6) {
        let det = m.get_download_detail(d.id).await.unwrap();
        let last = det.segments.len() - 1;
        let Some(first) = det.segments.iter().find(|s| s.index != last && s.downloaded < s.end - s.start + 1) else { break };
        match idle_since {
            Some((i, got, since)) if i == first.index && got == first.downloaded => {
                assert!(since.elapsed() < Duration::from_secs(4), "part {i} (the start) waits while others download: {:?}", det.segments);
            }
            _ => idle_since = Some((first.index, first.downloaded, Instant::now())),
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
