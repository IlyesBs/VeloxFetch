//! "Download again": a finished, cancelled or failed download fetched once more from the start, with the same
//! link, folder and name (for a file deleted by mistake). Never overwrites a file that is there.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use download_manager::{DownloadDto, DownloadManager, ManagerConfig, ManagerError, NewDownload};
use downloader_core::{AdaptiveConfig, DownloadStatus, RetryPolicy};
use velox_integration_tests::{random_data, serve, Server};

const MB: usize = 1024 * 1024;

struct Env {
    root: PathBuf,
    db: PathBuf,
}

fn env(name: &str) -> Env {
    let base = std::env::temp_dir().join(format!("velox-redl-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    Env { root: base.join("downloads"), db: base.join("state").join("velox.db") }
}

async fn manager(e: &Env) -> DownloadManager {
    let mut c = ManagerConfig::new(&e.db, &e.root);
    c.progress_interval = Duration::from_millis(40);
    c.download.min_segment_size = 64 * 1024;
    c.download.checkpoint_interval = Duration::from_millis(50);
    c.download.retry = RetryPolicy {
        max_retries: 1,
        max_throttle_retries: 1,
        base_delay: Duration::from_millis(5),
        max_delay: Duration::from_millis(20),
        max_retry_after: Duration::from_millis(50),
    };
    c.download.adaptive = AdaptiveConfig { interval: Duration::from_millis(150), ..Default::default() };
    DownloadManager::new(c).await.unwrap()
}

fn new_dl(srv: &Server, path: &str) -> NewDownload {
    NewDownload { url: srv.url(path), ..Default::default() }
}

async fn wait_status(m: &DownloadManager, id: i64, want: DownloadStatus) -> DownloadDto {
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
    while m.get_download(id).await.unwrap().downloaded < bytes {
        assert!(t.elapsed() < Duration::from_secs(20), "never reached {bytes} bytes");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn a_deleted_file_is_shown_missing_and_downloaded_again_in_place() {
    let data = random_data(2 * MB, 31);
    let srv = serve(data.clone()).await;
    let e = env("deleted");
    let m = manager(&e).await;
    let d = m.create_download(new_dl(&srv, "/movie.bin")).await.unwrap();
    let done = wait_status(&m, d.id, DownloadStatus::Completed).await;
    let path = e.root.join("movie.bin");
    assert!(!done.file_missing && path.exists());

    std::fs::remove_file(&path).unwrap(); // deleted by mistake, outside the app
    assert!(m.get_download(d.id).await.unwrap().file_missing);

    let again = m.redownload(d.id).await.unwrap();
    assert_eq!(again.id, d.id, "the same entry, not a new one");
    assert_eq!(again.status, DownloadStatus::Queued);
    let back = wait_status(&m, d.id, DownloadStatus::Completed).await;
    assert!(!back.file_missing);
    assert_eq!(back.destination, path.display().to_string(), "same folder and name");
    assert_eq!(std::fs::read(&path).unwrap(), *data, "byte-exact");
    assert!(!e.root.join("movie.bin.part").exists());
    assert_eq!(m.list_downloads(Default::default()).await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_file_that_is_still_there_is_never_overwritten() {
    let data = random_data(MB, 32);
    let srv = serve(data.clone()).await;
    let e = env("present");
    let m = manager(&e).await;
    let d = m.create_download(new_dl(&srv, "/doc.bin")).await.unwrap();
    wait_status(&m, d.id, DownloadStatus::Completed).await;
    let original = e.root.join("doc.bin");
    std::fs::write(&original, b"edited by the user").unwrap();

    m.redownload(d.id).await.unwrap();
    let back = wait_status(&m, d.id, DownloadStatus::Completed).await;
    assert_eq!(std::fs::read(&original).unwrap(), b"edited by the user", "kept as it was");
    let copy = e.root.join("doc (1).bin");
    assert_eq!(back.destination, copy.display().to_string());
    assert_eq!(std::fs::read(&copy).unwrap(), *data);
}

#[tokio::test]
async fn a_cancelled_download_starts_over_and_a_changed_file_is_fetched_fresh() {
    let data = random_data(6 * MB, 33);
    let srv = serve(data.clone()).await;
    *srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(8);
    let e = env("cancelled");
    let m = manager(&e).await;
    let d = m.create_download(new_dl(&srv, "/big.bin")).await.unwrap();
    wait_bytes(&m, d.id, MB as u64).await;
    assert_eq!(m.cancel_download(d.id).await.unwrap().status, DownloadStatus::Cancelled);

    // The server now has a new version: nothing of the old one may be kept.
    let fresh = random_data(3 * MB, 34);
    *srv.state.data.lock().unwrap() = fresh.clone();
    *srv.state.etag.lock().unwrap() = "\"v2\"".into();
    *srv.state.chunk_delay.lock().unwrap() = Duration::ZERO;
    let served = srv.state.bytes_served.load(Ordering::Relaxed);

    let again = m.redownload(d.id).await.unwrap();
    assert_eq!(again.downloaded, 0, "from the start");
    wait_status(&m, d.id, DownloadStatus::Completed).await;
    assert_eq!(std::fs::read(e.root.join("big.bin")).unwrap(), *fresh);
    assert!(srv.state.bytes_served.load(Ordering::Relaxed) - served >= fresh.len() as u64);
}

#[tokio::test]
async fn only_finished_cancelled_or_failed_downloads_can_be_downloaded_again() {
    let srv = serve(random_data(8 * MB, 35)).await;
    *srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(15);
    let e = env("states");
    let m = manager(&e).await;

    let running = m.create_download(new_dl(&srv, "/run.bin")).await.unwrap();
    wait_bytes(&m, running.id, MB as u64).await;
    assert!(matches!(m.redownload(running.id).await, Err(ManagerError::InvalidState { .. })), "running");
    let paused = m.pause_download(running.id).await.unwrap();
    assert_eq!(paused.status, DownloadStatus::Paused);
    assert!(matches!(m.redownload(running.id).await, Err(ManagerError::InvalidState { .. })), "paused keeps its progress");
    assert_eq!(m.get_download(running.id).await.unwrap().downloaded, paused.downloaded, "a refusal changes nothing");
    assert!(e.root.join("run.bin.part").exists());

    let later = m.create_download(NewDownload { start: Some(false), ..new_dl(&srv, "/later.bin") }).await.unwrap();
    assert!(matches!(m.redownload(later.id).await, Err(ManagerError::InvalidState { .. })), "not started yet");
    assert!(m.redownload(999_999).await.is_err(), "unknown id");

    // A failed one may: it runs again from the start (here it fails again: still not on the server).
    let failed = m.create_download(new_dl(&srv, "/404/gone.bin")).await.unwrap();
    wait_status(&m, failed.id, DownloadStatus::Failed).await;
    assert_eq!(m.redownload(failed.id).await.unwrap().status, DownloadStatus::Queued);
    let hits = srv.state.hits.load(Ordering::Relaxed);
    wait_status(&m, failed.id, DownloadStatus::Failed).await;
    assert!(srv.state.hits.load(Ordering::Relaxed) >= hits);
}
