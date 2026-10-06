//! Smart throttle and live preview, through the real manager and engine against a local server.
//! (The desktop app's process scan and its custom protocol are thin layers over what is tested here.)

use std::path::PathBuf;
use std::time::{Duration, Instant};

use download_manager::{DownloadManager, ManagerConfig, ManagerError, NewDownload, SetSmartThrottle};
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
async fn smart_throttle_slows_downloads_while_a_listed_program_runs_and_lifts_at_once() {
    let b = base("throttle");
    let m = manager(&b).await;
    let s = m.smart_throttle();
    assert!(!s.enabled && s.active_app.is_none() && s.limit_bps.is_none(), "off by default: {s:?}");
    assert!(s.apps.iter().any(|a| a == "League of Legends") && s.apps.iter().any(|a| a == "Zoom"));

    // Off: a listed program changes nothing.
    m.smart_throttle_observe(Some(&names(&["Zoom.exe"])));
    assert!(m.smart_throttle().limit_bps.is_none());

    m.set_smart_throttle(SetSmartThrottle { enabled: Some(true), apps: Some(names(&["VeloxTestGame"])), share_percent: Some(25) })
        .await
        .unwrap();
    m.smart_throttle_observe(Some(&names(&["explorer.exe", "VELOXTESTGAME.EXE"])));
    let s = m.smart_throttle();
    assert_eq!(s.active_app.as_deref(), Some("VeloxTestGame"));
    assert_eq!(s.limit_bps, Some(1024 * 1024), "before any speed is known: 1 MB/s");

    // A download started while the game runs is held to about 1 MB/s.
    let data = random_data(6 * MB, 7);
    let srv = serve(data.clone()).await;
    let d = m.create_download(NewDownload { url: srv.url("/big.bin"), ..Default::default() }).await.unwrap();
    tokio::time::sleep(Duration::from_millis(2000)).await;
    let mid = m.get_download(d.id).await.unwrap().downloaded;
    assert!(mid < 4 * MB as u64, "throttled: {mid} bytes after 2 s");
    assert!(mid > 0, "but still moving");

    // The game closes: full speed at once (not at the next restart of the download).
    m.smart_throttle_observe(Some(&names(&["explorer.exe"])));
    assert!(m.smart_throttle().limit_bps.is_none());
    let t = Instant::now();
    wait_status(&m, d.id, DownloadStatus::Completed, 10).await;
    assert!(t.elapsed() < Duration::from_secs(3), "the rest went at full speed: {:?}", t.elapsed());
    assert_eq!(std::fs::read(m.get_download(d.id).await.unwrap().destination).unwrap(), *data);

    // A failed scan never leaves downloads slow.
    m.smart_throttle_observe(Some(&names(&["VeloxTestGame.exe"])));
    assert!(m.smart_throttle().limit_bps.is_some());
    m.smart_throttle_observe(None);
    let s = m.smart_throttle();
    assert!(s.limit_bps.is_none() && !s.scan_ok, "{s:?}");

    // Turning it off lifts the cap immediately, without waiting for a scan.
    m.smart_throttle_observe(Some(&names(&["VeloxTestGame.exe"])));
    m.set_smart_throttle(SetSmartThrottle { enabled: Some(false), ..Default::default() }).await.unwrap();
    assert!(m.smart_throttle().limit_bps.is_none());
    m.shutdown().await;

    // The settings survive a restart; the cap does not (nothing is running yet).
    let m = manager(&b).await;
    let s = m.smart_throttle();
    assert!(!s.enabled && s.apps == names(&["VeloxTestGame"]) && s.share_percent == 25 && s.limit_bps.is_none(), "{s:?}");
    for bad in [
        SetSmartThrottle { share_percent: Some(0), ..Default::default() },
        SetSmartThrottle { share_percent: Some(95), ..Default::default() },
        SetSmartThrottle { apps: Some(names(&["C:\\Games\\x.exe"])), ..Default::default() },
    ] {
        assert!(matches!(m.set_smart_throttle(bad).await, Err(ManagerError::InvalidInput(_))));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_cap_follows_the_speed_this_connection_can_reach() {
    let b = base("learn");
    let m = manager(&b).await;
    m.set_smart_throttle(SetSmartThrottle { enabled: Some(true), apps: Some(names(&["VeloxTestCall"])), share_percent: Some(50) })
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
