//! Phase 4: the application controller (`DownloadManager`) that the Tauri commands call.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use download_manager::{DownloadManager, ListFilter, ManagerConfig, ManagerError, ManagerEvent, NewDownload, SetSpeedLimit, SpeedTarget};
use downloader_core::{AdaptiveConfig, DownloadStatus, Priority, RetryPolicy};
use velox_integration_tests::{random_data, serve, Server};

const MB: usize = 1024 * 1024;

struct Env {
    root: PathBuf,
    db: PathBuf,
}

fn env(name: &str) -> Env {
    let base = std::env::temp_dir().join(format!("velox-p4-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    Env { root: base.join("downloads"), db: base.join("state").join("velox.db") }
}

fn config(e: &Env, slots: usize) -> ManagerConfig {
    let mut c = ManagerConfig::new(&e.db, &e.root);
    c.max_concurrent_downloads = slots;
    c.progress_interval = Duration::from_millis(40);
    c.download.min_segment_size = 64 * 1024;
    c.download.checkpoint_interval = Duration::from_millis(50);
    c.download.retry = RetryPolicy {
        max_retries: 2,
        max_throttle_retries: 2,
        base_delay: Duration::from_millis(5),
        max_delay: Duration::from_millis(20),
        max_retry_after: Duration::from_millis(50),
    };
    c.download.adaptive = AdaptiveConfig { interval: Duration::from_millis(150), ..Default::default() };
    c
}

async fn manager(e: &Env, slots: usize) -> DownloadManager {
    DownloadManager::new(config(e, slots)).await.unwrap()
}

fn new_dl(srv: &Server, path: &str) -> NewDownload {
    NewDownload { url: srv.url(path), ..Default::default() }
}

async fn wait_status(m: &DownloadManager, id: i64, want: DownloadStatus) -> download_manager::DownloadDto {
    let t = Instant::now();
    loop {
        let d = m.get_download(id).await.unwrap();
        if d.status == want {
            return d;
        }
        assert!(t.elapsed() < Duration::from_secs(20), "download {id} stuck in {:?}, wanted {want:?} ({:?})", d.status, d.error);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_bytes(m: &DownloadManager, id: i64, bytes: u64) {
    let t = Instant::now();
    loop {
        if m.get_download(id).await.unwrap().downloaded >= bytes {
            return;
        }
        assert!(t.elapsed() < Duration::from_secs(20), "never reached {bytes} bytes");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

// ------------------------------------------------------------------ lifecycle

#[tokio::test]
async fn create_runs_to_completion_and_emits_events() {
    let data = random_data(3 * MB, 1);
    let srv = serve(data.clone()).await;
    *srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(4);
    let e = env("basic");
    let m = manager(&e, 3).await;
    let mut events = m.subscribe();

    let dto = m.create_download(new_dl(&srv, "/movie.bin")).await.unwrap();
    assert_eq!(dto.status, DownloadStatus::Queued);
    assert_eq!(dto.filename, "movie.bin", "name shown from the URL before the server is asked");
    let done = wait_status(&m, dto.id, DownloadStatus::Completed).await;
    assert_eq!((done.downloaded, done.total_size), (3 * MB as u64, Some(3 * MB as u64)));
    assert_eq!(std::fs::read(e.root.join("movie.bin")).unwrap(), *data);

    // The database shows "completed" a few milliseconds before the manager announces it, so wait
    // for that event (bounded) instead of sampling the channel once.
    let mut seen = Vec::new();
    let is_done = |e: &ManagerEvent| matches!(e, ManagerEvent::Updated { download } if download.id == dto.id && download.status == DownloadStatus::Completed);
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while !seen.iter().any(is_done) && std::time::Instant::now() < deadline {
        if let Ok(Ok(ev)) = tokio::time::timeout(Duration::from_millis(100), events.recv()).await {
            seen.push(ev);
        }
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    while let Ok(ev) = events.try_recv() {
        seen.push(ev);
    }
    assert!(matches!(seen.first(), Some(ManagerEvent::Added { .. })));
    assert!(seen
        .iter()
        .any(|e| matches!(e, ManagerEvent::Tick { tick } if tick.downloads.iter().any(|p| p.id == dto.id && p.downloaded > 0))));
    // The completed update must be there, and after it only the aggregator's "settling" ticks may
    // follow (they are sent for a moment after the last download stops, so the UI shows zeros).
    let completed = seen
        .iter()
        .rposition(
            |e| matches!(e, ManagerEvent::Updated { download } if download.id == dto.id && download.status == DownloadStatus::Completed),
        )
        .expect("an Updated(Completed) event");
    assert!(seen[completed + 1..].iter().all(|e| matches!(e, ManagerEvent::Tick { .. })), "only settling ticks may follow the completion");
}

#[tokio::test]
async fn created_without_start_stays_queued_until_started() {
    let srv = serve(random_data(MB, 2)).await;
    let e = env("nostart");
    let m = manager(&e, 3).await;
    let mut req = new_dl(&srv, "/a.bin");
    req.start = Some(false);
    let dto = m.create_download(req).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(m.get_download(dto.id).await.unwrap().status, DownloadStatus::Queued);
    assert_eq!(srv.state.hits.load(std::sync::atomic::Ordering::SeqCst), 0);
    m.start_download(dto.id).await.unwrap();
    m.start_download(dto.id).await.unwrap(); // idempotent while running
    wait_status(&m, dto.id, DownloadStatus::Completed).await;
    assert!(e.root.join("a.bin").exists());
}

#[tokio::test]
async fn subdir_priority_category_and_limit_are_applied() {
    let srv = serve(random_data(MB, 3)).await;
    let e = env("opts");
    let m = manager(&e, 3).await;
    let mut req = new_dl(&srv, "/x.bin");
    req.subdir = Some("videos/2026".into());
    req.filename = Some("custom.bin".into());
    req.priority = Some(Priority::High);
    req.category = Some("video".into());
    let dto = m.create_download(req).await.unwrap();
    assert_eq!((dto.priority, dto.category.as_deref()), (Priority::High, Some("video")));
    wait_status(&m, dto.id, DownloadStatus::Completed).await;
    assert!(e.root.join("videos").join("2026").join("custom.bin").exists());
}

#[tokio::test]
async fn pause_then_resume_finishes_the_file() {
    let data = random_data(8 * MB, 4);
    let srv = serve(data.clone()).await;
    *srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(15);
    let e = env("pause");
    let m = manager(&e, 3).await;
    let dto = m.create_download(new_dl(&srv, "/big.bin")).await.unwrap();
    wait_bytes(&m, dto.id, 2 * MB as u64).await;

    let paused = m.pause_download(dto.id).await.unwrap();
    assert_eq!(paused.status, DownloadStatus::Paused);
    assert!(paused.downloaded >= 2 * MB as u64 && paused.downloaded < data.len() as u64);
    assert!(e.root.join("big.bin.part").exists() && !e.root.join("big.bin").exists());
    assert_eq!(paused.speed_bps, 0);
    // Pausing again is harmless, and resuming works.
    assert!(m.pause_download(dto.id).await.is_ok(), "pausing an already paused download is a no-op");
    *srv.state.chunk_delay.lock().unwrap() = Duration::ZERO;
    m.resume_download(dto.id).await.unwrap();
    wait_status(&m, dto.id, DownloadStatus::Completed).await;
    assert_eq!(std::fs::read(e.root.join("big.bin")).unwrap(), *data);
}

#[tokio::test]
async fn cancel_discards_data_and_cannot_be_resumed() {
    let srv = serve(random_data(8 * MB, 5)).await;
    *srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(15);
    let e = env("cancel");
    let m = manager(&e, 3).await;
    let dto = m.create_download(new_dl(&srv, "/c.bin")).await.unwrap();
    wait_bytes(&m, dto.id, MB as u64).await;

    let c = m.cancel_download(dto.id).await.unwrap();
    assert_eq!(c.status, DownloadStatus::Cancelled);
    assert!(!e.root.join("c.bin.part").exists() && !e.root.join("c.bin").exists());
    assert!(matches!(m.resume_download(dto.id).await, Err(ManagerError::InvalidState { .. })));
    assert!(matches!(m.start_download(dto.id).await, Err(ManagerError::InvalidState { .. })));
    assert!(matches!(m.cancel_download(dto.id).await, Err(ManagerError::InvalidState { .. })));
    assert!(matches!(m.retry_download(dto.id).await, Err(ManagerError::InvalidState { .. })));
}

#[tokio::test]
async fn a_paused_download_can_be_cancelled() {
    let srv = serve(random_data(8 * MB, 6)).await;
    *srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(15);
    let e = env("cancelpaused");
    let m = manager(&e, 3).await;
    let dto = m.create_download(new_dl(&srv, "/p.bin")).await.unwrap();
    wait_bytes(&m, dto.id, MB as u64).await;
    m.pause_download(dto.id).await.unwrap();
    assert!(e.root.join("p.bin.part").exists());
    assert_eq!(m.cancel_download(dto.id).await.unwrap().status, DownloadStatus::Cancelled);
    assert!(!e.root.join("p.bin.part").exists());
}

#[tokio::test]
async fn failures_are_recorded_and_can_be_retried() {
    let srv = serve(random_data(MB, 7)).await;
    let e = env("fail");
    let m = manager(&e, 3).await;
    let dto = m.create_download(new_dl(&srv, "/404/missing.bin")).await.unwrap();
    let failed = wait_status(&m, dto.id, DownloadStatus::Failed).await;
    assert!(failed.error.as_deref().is_some_and(|e| e.contains("not on the server")), "{:?}", failed.error);
    assert!(failed.error_detail.as_deref().is_some_and(|e| e.contains("404")), "{:?}", failed.error_detail);

    // Retry is only valid for failed downloads, and runs again (the file is still missing).
    let again = m.retry_download(dto.id).await.unwrap();
    assert_eq!(again.status, DownloadStatus::Queued);
    wait_status(&m, dto.id, DownloadStatus::Failed).await;
    let ok = m.create_download(new_dl(&srv, "/good.bin")).await.unwrap();
    wait_status(&m, ok.id, DownloadStatus::Completed).await;
    assert!(matches!(m.retry_download(ok.id).await, Err(ManagerError::InvalidState { .. })));
}

#[tokio::test]
async fn delete_removes_records_partials_and_optionally_the_file() {
    let data = random_data(4 * MB, 8);
    let srv = serve(data).await;
    *srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(10);
    let e = env("delete");
    let m = manager(&e, 3).await;
    let mut events = m.subscribe();

    // Running download: delete cancels it, removes the .part and the record.
    let run = m.create_download(new_dl(&srv, "/run.bin")).await.unwrap();
    wait_bytes(&m, run.id, MB as u64).await;
    m.delete_download(run.id, false).await.unwrap();
    assert!(!e.root.join("run.bin.part").exists());
    assert!(matches!(m.get_download(run.id).await, Err(ManagerError::NotFound(_))));

    // Completed: the file survives a plain delete, but goes with delete_file = true.
    *srv.state.chunk_delay.lock().unwrap() = Duration::ZERO;
    let a = m.create_download(new_dl(&srv, "/keep.bin")).await.unwrap();
    wait_status(&m, a.id, DownloadStatus::Completed).await;
    m.delete_download(a.id, false).await.unwrap();
    assert!(e.root.join("keep.bin").exists());
    let b = m.create_download(new_dl(&srv, "/gone.bin")).await.unwrap();
    wait_status(&m, b.id, DownloadStatus::Completed).await;
    m.delete_download(b.id, true).await.unwrap();
    assert!(!e.root.join("gone.bin").exists());
    assert!(m.list_downloads(ListFilter::default()).await.unwrap().is_empty());

    let mut removed = 0;
    while let Ok(ev) = events.try_recv() {
        removed += matches!(ev, ManagerEvent::Removed { .. }) as usize;
    }
    assert_eq!(removed, 3);
}

// -------------------------------------------------------------- queue / limits

#[tokio::test]
async fn concurrency_limit_queues_downloads_and_priority_reorders_them() {
    let srv = serve(random_data(2 * MB, 9)).await;
    // Slow enough that `first` still runs 150 ms later, also where timers are exact (see phase3's priority test).
    *srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(40);
    let e = env("queue");
    let m = manager(&e, 1).await;

    let first = m.create_download(new_dl(&srv, "/first.bin")).await.unwrap();
    wait_status(&m, first.id, DownloadStatus::Downloading).await;
    let low = m.create_download(new_dl(&srv, "/low.bin")).await.unwrap();
    let late = m.create_download(new_dl(&srv, "/late.bin")).await.unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    let listed = m.list_downloads(ListFilter { status: Some(vec![DownloadStatus::Queued]) }).await.unwrap();
    assert_eq!(listed.len(), 2, "one slot: the others wait");

    // Bump the later download ahead of the earlier one.
    let bumped = m.set_priority(late.id, Priority::Highest).await.unwrap();
    assert_eq!(bumped.priority, Priority::Highest);

    wait_status(&m, late.id, DownloadStatus::Downloading).await;
    assert_eq!(m.get_download(low.id).await.unwrap().status, DownloadStatus::Queued, "low waits for late");
    for id in [first.id, low.id, late.id] {
        wait_status(&m, id, DownloadStatus::Completed).await;
    }
}

#[tokio::test]
async fn speed_limits_apply_persist_and_are_validated() {
    let srv = serve(random_data(MB, 10)).await;
    let e = env("limits");
    {
        let m = manager(&e, 3).await;
        // Global limit is stored and enforced.
        m.set_speed_limit(SetSpeedLimit { target: SpeedTarget::Global, bytes_per_sec: Some(512 * 1024) }).await.unwrap();
        m.set_speed_limit(SetSpeedLimit { target: SpeedTarget::Category { name: "slow".into() }, bytes_per_sec: Some(256 * 1024) })
            .await
            .unwrap();
        let t = Instant::now();
        let d = m.create_download(new_dl(&srv, "/g.bin")).await.unwrap();
        wait_status(&m, d.id, DownloadStatus::Completed).await;
        assert!(t.elapsed() >= Duration::from_millis(1300), "global limit ignored: {:?}", t.elapsed());

        // Bad values are rejected.
        for bad in [Some(0), Some(5), Some(u64::MAX)] {
            let r = m.set_speed_limit(SetSpeedLimit { target: SpeedTarget::Global, bytes_per_sec: bad }).await;
            assert!(matches!(r, Err(ManagerError::InvalidInput(_))), "{bad:?}");
        }
        let r = m
            .set_speed_limit(SetSpeedLimit { target: SpeedTarget::Category { name: "bad name!".into() }, bytes_per_sec: Some(4096) })
            .await;
        assert!(matches!(r, Err(ManagerError::InvalidInput(_))));
        let r = m.set_speed_limit(SetSpeedLimit { target: SpeedTarget::Download { id: 999 }, bytes_per_sec: Some(4096) }).await;
        assert!(matches!(r, Err(ManagerError::NotFound(999))));
    }
    // A fresh manager on the same database restores the limits...
    let m = manager(&e, 3).await;
    let t = Instant::now();
    let d = m.create_download(new_dl(&srv, "/g2.bin")).await.unwrap();
    wait_status(&m, d.id, DownloadStatus::Completed).await;
    assert!(t.elapsed() >= Duration::from_millis(1300), "global limit not restored after restart");
    // ...until it is cleared.
    m.set_speed_limit(SetSpeedLimit { target: SpeedTarget::Global, bytes_per_sec: None }).await.unwrap();
    let t = Instant::now();
    let d = m.create_download(new_dl(&srv, "/g3.bin")).await.unwrap();
    wait_status(&m, d.id, DownloadStatus::Completed).await;
    assert!(t.elapsed() < Duration::from_millis(1200));
}

#[tokio::test]
async fn per_download_limit_can_change_while_running() {
    let srv = serve(random_data(3 * MB, 11)).await;
    let e = env("dllimit");
    let m = manager(&e, 3).await;
    let mut req = new_dl(&srv, "/slow.bin");
    req.speed_limit = Some(128 * 1024); // ~20 s at this rate
    let d = m.create_download(req).await.unwrap();
    wait_status(&m, d.id, DownloadStatus::Downloading).await;
    assert_eq!(m.get_download(d.id).await.unwrap().speed_limit, Some(128 * 1024));
    tokio::time::sleep(Duration::from_millis(400)).await;
    m.set_speed_limit(SetSpeedLimit { target: SpeedTarget::Download { id: d.id }, bytes_per_sec: None }).await.unwrap();
    let t = Instant::now();
    let done = wait_status(&m, d.id, DownloadStatus::Completed).await;
    assert!(t.elapsed() < Duration::from_secs(5));
    assert_eq!(done.speed_limit, None);
}

// ------------------------------------------------------- validation / security

#[tokio::test]
async fn hostile_requests_are_rejected_and_leave_no_trace() {
    let srv = serve(random_data(1000, 12)).await;
    let e = env("hostile");
    let m = manager(&e, 3).await;
    let bad: Vec<NewDownload> = vec![
        NewDownload { url: "file:///C:/Windows/System32/config/SAM".into(), ..Default::default() },
        NewDownload { url: "javascript:alert(1)".into(), ..Default::default() },
        NewDownload { subdir: Some("../../outside".into()), ..new_dl(&srv, "/a.bin") },
        NewDownload { subdir: Some("C:\\Windows".into()), ..new_dl(&srv, "/a.bin") },
        NewDownload { subdir: Some("/etc".into()), ..new_dl(&srv, "/a.bin") },
        NewDownload { filename: Some("..\\..\\evil.exe".into()), ..new_dl(&srv, "/a.bin") },
        NewDownload { filename: Some("a/b".into()), ..new_dl(&srv, "/a.bin") },
        NewDownload { category: Some("no spaces allowed".into()), ..new_dl(&srv, "/a.bin") },
        NewDownload { speed_limit: Some(3), ..new_dl(&srv, "/a.bin") },
        NewDownload { checksum: Some("md5:abc".into()), ..new_dl(&srv, "/a.bin") },
    ];
    for r in bad {
        let desc = format!("{r:?}");
        assert!(matches!(m.create_download(r).await, Err(ManagerError::InvalidInput(_))), "accepted: {desc}");
    }
    assert!(m.list_downloads(ListFilter::default()).await.unwrap().is_empty(), "rejected requests must not be stored");
    assert_eq!(srv.state.hits.load(std::sync::atomic::Ordering::SeqCst), 0, "rejected requests must not touch the network");
    assert!(!e.root.parent().unwrap().join("outside").exists());
}

#[test]
fn unknown_fields_and_bad_enum_values_are_rejected_at_the_boundary() {
    let ok = serde_json::from_str::<NewDownload>(r#"{"url":"https://a.b/c","priority":"high","subdir":"x"}"#);
    assert!(ok.is_ok());
    for bad in [
        r#"{"url":"https://a.b/c","execute":"calc.exe"}"#,
        r#"{"url":"https://a.b/c","priority":"urgent"}"#,
        r#"{"url":"https://a.b/c","speedLimit":-5}"#,
        r#"{"url":42}"#,
        r#"{}"#,
    ] {
        assert!(serde_json::from_str::<NewDownload>(bad).is_err(), "accepted {bad}");
    }
    assert!(serde_json::from_str::<SetSpeedLimit>(r#"{"target":{"kind":"global"},"bytesPerSec":1000000}"#).is_ok());
    assert!(serde_json::from_str::<SetSpeedLimit>(r#"{"target":{"kind":"shell","cmd":"x"},"bytesPerSec":1}"#).is_err());
    let dto_json = serde_json::to_value(download_manager::ProgressDto {
        id: 1,
        downloaded: 2,
        total_size: Some(3),
        speed_bps: 4,
        eta_secs: None,
        connections: 5,
        retrying: false,
    })
    .unwrap();
    assert!(dto_json.get("speedBps").is_some() && dto_json.get("totalSize").is_some(), "camelCase for the UI: {dto_json}");
}

#[tokio::test]
async fn error_shape_sent_to_the_ui_is_terse() {
    let e = env("apierr");
    let m = manager(&e, 3).await;
    let err = m.get_download(4242).await.unwrap_err();
    let api = download_manager::ApiError::from(err);
    assert_eq!(api.code, "not_found");
    assert!(!api.message.contains(&*e.root.to_string_lossy()), "no filesystem paths in errors");
}

// ------------------------------------------------- restart / shutdown / recovery

#[tokio::test]
async fn shutdown_pauses_everything_and_a_new_manager_can_resume() {
    let data = random_data(8 * MB, 13);
    let srv = serve(data.clone()).await;
    *srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(15);
    let e = env("shutdown");
    let id = {
        let m = manager(&e, 3).await;
        let d = m.create_download(new_dl(&srv, "/s.bin")).await.unwrap();
        wait_bytes(&m, d.id, 2 * MB as u64).await;
        m.shutdown().await;
        assert_eq!(m.get_download(d.id).await.unwrap().status, DownloadStatus::Paused);
        d.id
    };
    // "Restart the app".
    *srv.state.chunk_delay.lock().unwrap() = Duration::ZERO;
    let m = manager(&e, 3).await;
    let listed = m.list_downloads(ListFilter::default()).await.unwrap();
    assert_eq!((listed.len(), listed[0].status), (1, DownloadStatus::Paused));
    assert!(listed[0].downloaded >= 2 * MB as u64, "progress survived the restart");
    m.resume_download(id).await.unwrap();
    wait_status(&m, id, DownloadStatus::Completed).await;
    assert_eq!(std::fs::read(e.root.join("s.bin")).unwrap(), *data);
}

#[tokio::test]
async fn a_crashed_download_is_recovered_at_startup() {
    let data = random_data(8 * MB, 14);
    let srv = serve(data.clone()).await;
    *srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(15);
    let e = env("crash");

    // Run the first "process" on its own runtime and kill it abruptly: no pause, no final
    // checkpoint, exactly what a crash or power cut leaves behind.
    let (cfg, url) = (config(&e, 3), srv.url("/x.bin"));
    let id = tokio::task::spawn_blocking(move || {
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        let id = rt.block_on(async {
            let m = DownloadManager::new(cfg).await.unwrap();
            let d = m.create_download(NewDownload { url, ..Default::default() }).await.unwrap();
            wait_bytes(&m, d.id, 3 * MB as u64).await;
            tokio::time::sleep(Duration::from_millis(150)).await; // let a checkpoint land
            d.id
        });
        rt.shutdown_background(); // tasks are dropped mid-flight
        id
    })
    .await
    .unwrap();
    *srv.state.chunk_delay.lock().unwrap() = Duration::ZERO;

    // The database still says "downloading": that is the crash signature.
    let raw = persistence::SqliteStore::open(&e.db).unwrap();
    let stale = downloader_core::DownloadStore::load(&raw, id).unwrap().unwrap();
    assert_eq!(stale.status, DownloadStatus::Downloading, "crash should leave the stale status");
    drop(raw);

    // "Restart the app": startup recovery turns it into a resumable paused download.
    let m = manager(&e, 3).await;
    let d = m.get_download(id).await.unwrap();
    assert_eq!(d.status, DownloadStatus::Paused);
    assert!(d.downloaded > 0 && d.downloaded < data.len() as u64, "checkpointed progress kept: {}", d.downloaded);
    m.resume_download(id).await.unwrap();
    wait_status(&m, id, DownloadStatus::Completed).await;
    assert_eq!(std::fs::read(e.root.join("x.bin")).unwrap(), *data);
}
