use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use downloader_core::{
    part_path, Checksum, ClientConfig, DownloadConfig, DownloadControl, DownloadError, DownloadRequest, DownloadStatus, DownloadStore,
    Downloader, ProgressHandle, RetryPolicy, SegmentStatus,
};
use persistence::{RecoveryAction, SqliteStore};
use sha2::{Digest, Sha256};
use velox_integration_tests::{random_data, serve};

const MB: usize = 1024 * 1024;

fn tmpdir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("velox-it-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

fn fast_retry(max: u32) -> RetryPolicy {
    RetryPolicy {
        max_retries: max,
        max_throttle_retries: 3,
        base_delay: Duration::from_millis(5),
        max_delay: Duration::from_millis(20),
        max_retry_after: Duration::from_millis(50),
    }
}

fn downloader(store: Option<Arc<SqliteStore>>, retries: u32) -> Downloader {
    let cfg = DownloadConfig {
        min_segment_size: 64 * 1024,
        retry: fast_retry(retries),
        checkpoint_interval: Duration::from_millis(50),
        ..Default::default()
    };
    let d = Downloader::new(&ClientConfig::default(), cfg).unwrap();
    match store {
        Some(s) => d.with_store(s),
        None => d,
    }
}

async fn wait_until(p: &ProgressHandle, bytes: u64) {
    for _ in 0..2000 {
        if p.snapshot().downloaded >= bytes {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("progress never reached {bytes}");
}

fn sha256(d: &[u8]) -> String {
    Sha256::digest(d).iter().map(|b| format!("{b:02x}")).collect()
}

// ---------------------------------------------------------------- retry engine

#[tokio::test]
async fn transient_503s_are_retried_and_download_is_exact() {
    let data = random_data(4 * MB, 1);
    let srv = serve(data.clone()).await;
    srv.state.fail_next.store(6, Ordering::SeqCst);
    let dir = tmpdir("retry503");
    let o = downloader(None, 8)
        .download(&DownloadRequest::new(srv.url("/f.bin"), &dir), DownloadControl::new(), ProgressHandle::new())
        .await
        .unwrap();
    assert_eq!(std::fs::read(o.path).unwrap(), *data);
}

#[tokio::test]
async fn mid_body_disconnects_resume_from_the_written_offset() {
    let data = random_data(4 * MB, 2);
    let srv = serve(data.clone()).await;
    srv.state.truncate_next.store(4, Ordering::SeqCst);
    let dir = tmpdir("truncate");
    let progress = ProgressHandle::new();
    let o = downloader(None, 8)
        .download(&DownloadRequest::new(srv.url("/f.bin"), &dir), DownloadControl::new(), progress.clone())
        .await
        .unwrap();
    assert_eq!(std::fs::read(o.path).unwrap(), *data);
    assert!(progress.snapshot().segments.iter().any(|s| s.retry_count > 0), "no segment recorded a retry");
    // Resuming at the offset (not restarting the segment) keeps re-served bytes well below 2x.
    assert!(srv.state.bytes_served.load(Ordering::SeqCst) < (data.len() as u64) * 3 / 2 + 1);
}

#[tokio::test]
async fn gives_up_after_max_retries_and_never_retries_404() {
    let srv = serve(random_data(2 * MB, 3)).await;
    srv.state.fail_next.store(1000, Ordering::SeqCst);
    let dir = tmpdir("giveup");
    let e = downloader(None, 2)
        .download(&DownloadRequest::new(srv.url("/f.bin"), &dir), DownloadControl::new(), ProgressHandle::new())
        .await
        .unwrap_err();
    assert!(e.to_string().contains("503"), "{e}");

    let e = downloader(None, 5)
        .download(&DownloadRequest::new(srv.url("/404"), &dir), DownloadControl::new(), ProgressHandle::new())
        .await
        .unwrap_err();
    assert!(e.to_string().contains("404"), "{e}");
}

// ------------------------------------------------------------ pause / resume

#[tokio::test]
async fn pause_persists_state_and_resume_finishes_without_redownloading() {
    let data = random_data(8 * MB, 4);
    let srv = serve(data.clone()).await;
    *srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(15);
    let dir = tmpdir("pause");
    let store = Arc::new(SqliteStore::open(dir.join("state.db")).unwrap_or_else(|_| {
        std::fs::create_dir_all(&dir).unwrap();
        SqliteStore::open(dir.join("state.db")).unwrap()
    }));
    let dl = downloader(Some(store.clone()), 3);

    let (token, progress) = (DownloadControl::new(), ProgressHandle::new());
    let (d2, t2, p2, url, dir2) = (dl.clone(), token.clone(), progress.clone(), srv.url("/f.bin"), dir.clone());
    let task = tokio::spawn(async move { d2.download(&DownloadRequest::new(url, &dir2), t2, p2).await });
    wait_until(&progress, 2 * MB as u64).await;
    token.pause();
    assert!(matches!(task.await.unwrap(), Err(DownloadError::Paused)));

    let id = progress.download_id().expect("id assigned");
    let rec = store.load(id).unwrap().unwrap();
    assert_eq!(rec.status, DownloadStatus::Paused);
    assert!(rec.downloaded >= 2 * MB as u64 && rec.downloaded < data.len() as u64, "{}", rec.downloaded);
    assert!(part_path(&rec.destination).exists() && !rec.destination.exists());
    assert!(rec.segments.iter().any(|s| s.status != SegmentStatus::Completed));

    // Resume with a brand-new Downloader (as after an app restart).
    *srv.state.chunk_delay.lock().unwrap() = Duration::ZERO;
    let served_before = srv.state.bytes_served.load(Ordering::SeqCst);
    let o = downloader(Some(store.clone()), 3)
        .download(&DownloadRequest::resume(id), DownloadControl::new(), ProgressHandle::new())
        .await
        .unwrap();
    assert!(o.resumed);
    assert_eq!(std::fs::read(&o.path).unwrap(), *data);
    let served_after = srv.state.bytes_served.load(Ordering::SeqCst) - served_before;
    let remaining = data.len() as u64 - rec.downloaded;
    assert!(served_after <= remaining + 512 * 1024, "re-downloaded too much: {served_after} > {remaining}");
    assert_eq!(store.load(id).unwrap().unwrap().status, DownloadStatus::Completed);
    assert_eq!(store.history(5).unwrap().len(), 1);
}

#[tokio::test]
async fn changed_etag_forces_a_clean_restart() {
    let data = random_data(4 * MB, 5);
    let srv = serve(data.clone()).await;
    *srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(15);
    let dir = tmpdir("etag");
    std::fs::create_dir_all(&dir).unwrap();
    let store = Arc::new(SqliteStore::open(dir.join("s.db")).unwrap());

    let (token, progress) = (DownloadControl::new(), ProgressHandle::new());
    let (d2, t2, p2, url, dir2) = (downloader(Some(store.clone()), 3), token.clone(), progress.clone(), srv.url("/f.bin"), dir.clone());
    let task = tokio::spawn(async move { d2.download(&DownloadRequest::new(url, &dir2), t2, p2).await });
    wait_until(&progress, MB as u64).await;
    token.pause();
    let _ = task.await.unwrap();
    let id = progress.download_id().unwrap();

    // The file changes on the server: new content, new ETag, same size.
    let new_data = random_data(4 * MB, 6);
    srv.set_data(new_data.clone());
    *srv.state.etag.lock().unwrap() = "\"v2\"".into();
    *srv.state.chunk_delay.lock().unwrap() = Duration::ZERO;

    let o = downloader(Some(store.clone()), 3)
        .download(&DownloadRequest::resume(id), DownloadControl::new(), ProgressHandle::new())
        .await
        .unwrap();
    assert!(!o.resumed, "must not splice old bytes into a changed file");
    assert_eq!(std::fs::read(&o.path).unwrap(), *new_data);
}

// ------------------------------------------------------------- crash recovery

#[tokio::test]
async fn crash_mid_download_then_recover_and_resume() {
    let data = random_data(8 * MB, 7);
    let srv = serve(data.clone()).await;
    *srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(15);
    let dir = tmpdir("crash");
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("s.db");

    let progress = ProgressHandle::new();
    {
        let store = Arc::new(SqliteStore::open(&db).unwrap());
        let (dl, p2, url, dir2) = (downloader(Some(store), 3), progress.clone(), srv.url("/f.bin"), dir.clone());
        let task = tokio::spawn(async move {
            let _ = dl.download(&DownloadRequest::new(url, &dir2), DownloadControl::new(), p2).await;
        });
        wait_until(&progress, 3 * MB as u64).await;
        tokio::time::sleep(Duration::from_millis(150)).await; // let a checkpoint land
        task.abort(); // "crash": no pause, no final checkpoint, status stays `Downloading`
        let _ = task.await;
    }
    *srv.state.chunk_delay.lock().unwrap() = Duration::ZERO;
    let id = progress.download_id().unwrap();

    // "Restart the application": fresh connection to the same database file.
    let store = Arc::new(SqliteStore::open(&db).unwrap());
    let before = store.load(id).unwrap().unwrap();
    assert_eq!(before.status, DownloadStatus::Downloading, "crash should leave the stale status");
    assert!(before.downloaded > 0, "expected a persisted checkpoint");

    let reports = store.recover_interrupted().unwrap();
    assert_eq!(reports.len(), 1);
    assert!(matches!(reports[0].action, RecoveryAction::ResumeReady { downloaded } if downloaded > 0), "{:?}", reports[0]);

    let o = downloader(Some(store.clone()), 3)
        .download(&DownloadRequest::resume(id), DownloadControl::new(), ProgressHandle::new())
        .await
        .unwrap();
    assert!(o.resumed);
    assert_eq!(std::fs::read(&o.path).unwrap(), *data);
}

#[tokio::test]
async fn crash_with_lost_part_file_restarts_cleanly() {
    let data = random_data(6 * MB, 8);
    let srv = serve(data.clone()).await;
    *srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(15);
    let dir = tmpdir("crash2");
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("s.db");
    let progress = ProgressHandle::new();
    {
        let store = Arc::new(SqliteStore::open(&db).unwrap());
        let (dl, p2, url, dir2) = (downloader(Some(store), 3), progress.clone(), srv.url("/f.bin"), dir.clone());
        let task = tokio::spawn(async move {
            let _ = dl.download(&DownloadRequest::new(url, &dir2), DownloadControl::new(), p2).await;
        });
        wait_until(&progress, 2 * MB as u64).await;
        tokio::time::sleep(Duration::from_millis(150)).await;
        task.abort();
        let _ = task.await;
    }
    *srv.state.chunk_delay.lock().unwrap() = Duration::ZERO;
    let id = progress.download_id().unwrap();
    let store = Arc::new(SqliteStore::open(&db).unwrap());
    let rec = store.load(id).unwrap().unwrap();
    let _ = std::fs::remove_file(part_path(&rec.destination)); // disk lost the partial file

    let r = store.recover_interrupted().unwrap();
    assert!(matches!(r[0].action, RecoveryAction::Restart { .. }));
    let o = downloader(Some(store), 3).download(&DownloadRequest::resume(id), DownloadControl::new(), ProgressHandle::new()).await.unwrap();
    assert!(!o.resumed);
    assert_eq!(std::fs::read(&o.path).unwrap(), *data);
}

// ------------------------------------------------------------------ integrity

#[tokio::test]
async fn checksum_is_verified_and_mismatch_leaves_no_file() {
    let data = random_data(2 * MB, 9);
    let srv = serve(data.clone()).await;
    let dir = tmpdir("sum");

    let mut req = DownloadRequest::new(srv.url("/ok.bin"), &dir);
    req.expected_checksum = Some(Checksum::parse(&format!("sha256:{}", sha256(&data))).unwrap());
    let o = downloader(None, 2).download(&req, DownloadControl::new(), ProgressHandle::new()).await.unwrap();
    assert!(o.checksum_verified);

    let mut bad = DownloadRequest::new(srv.url("/bad.bin"), &dir);
    bad.expected_checksum = Some(Checksum::parse(&format!("sha256:{}", "0".repeat(64))).unwrap());
    let e = downloader(None, 2).download(&bad, DownloadControl::new(), ProgressHandle::new()).await.unwrap_err();
    assert!(matches!(e, DownloadError::ChecksumMismatch { .. }), "{e}");
    assert!(!Path::new(&dir.join("bad.bin")).exists());
    assert!(!part_path(&dir.join("bad.bin")).exists(), "unverified data must be deleted");
}

// ------------------------------------------------------------ cancel (!= pause)

async fn slow_store_download(name: &str) -> (velox_integration_tests::Server, PathBuf, Arc<SqliteStore>, Downloader) {
    let srv = serve(random_data(8 * MB, 10)).await;
    *srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(15);
    let dir = tmpdir(name);
    std::fs::create_dir_all(&dir).unwrap();
    let store = Arc::new(SqliteStore::open(dir.join("s.db")).unwrap());
    let dl = downloader(Some(store.clone()), 3);
    (srv, dir, store, dl)
}

#[tokio::test]
async fn cancel_discards_partial_data_and_cannot_be_resumed() {
    let (srv, dir, store, dl) = slow_store_download("cancel").await;
    let (control, progress) = (DownloadControl::new(), ProgressHandle::new());
    let (d2, c2, p2, url, dir2) = (dl.clone(), control.clone(), progress.clone(), srv.url("/f.bin"), dir.clone());
    let task = tokio::spawn(async move { d2.download(&DownloadRequest::new(url, &dir2), c2, p2).await });
    wait_until(&progress, 2 * MB as u64).await;
    control.cancel();
    let err = task.await.unwrap().unwrap_err();
    assert!(matches!(err, DownloadError::Cancelled), "{err}");

    let id = progress.download_id().unwrap();
    let rec = store.load(id).unwrap().unwrap();
    assert_eq!(rec.status, DownloadStatus::Cancelled);
    assert_eq!(rec.downloaded, 0);
    assert!(!part_path(&rec.destination).exists(), ".part must be deleted");
    assert!(!rec.destination.exists());
    assert!(store.history(5).unwrap().is_empty(), "a cancelled download is not a completed one");

    let e = dl.download(&DownloadRequest::resume(id), DownloadControl::new(), ProgressHandle::new()).await.unwrap_err();
    assert!(matches!(e, DownloadError::InvalidRequest(_)), "{e}");
    // Crash recovery must leave cancelled downloads alone.
    assert!(store.recover_interrupted().unwrap().is_empty());
}

#[tokio::test]
async fn a_paused_download_can_be_cancelled_by_id() {
    let (srv, dir, store, dl) = slow_store_download("cancelpaused").await;
    let (control, progress) = (DownloadControl::new(), ProgressHandle::new());
    let (d2, c2, p2, url, dir2) = (dl.clone(), control.clone(), progress.clone(), srv.url("/f.bin"), dir.clone());
    let task = tokio::spawn(async move { d2.download(&DownloadRequest::new(url, &dir2), c2, p2).await });
    wait_until(&progress, MB as u64).await;
    control.pause();
    assert!(matches!(task.await.unwrap(), Err(DownloadError::Paused)));
    let id = progress.download_id().unwrap();
    let rec = store.load(id).unwrap().unwrap();
    assert!(part_path(&rec.destination).exists(), "pause keeps the partial file");

    dl.cancel(id).await.unwrap();
    let rec = store.load(id).unwrap().unwrap();
    assert_eq!(rec.status, DownloadStatus::Cancelled);
    assert!(!part_path(&rec.destination).exists());
    assert!(matches!(dl.cancel(9999).await, Err(DownloadError::InvalidRequest(_))));
}

#[tokio::test]
async fn cancel_wins_over_pause_and_works_without_a_store() {
    let (srv, dir, _store, _dl) = slow_store_download("cancelwins").await;
    let plain = downloader(None, 3);
    let (control, progress) = (DownloadControl::new(), ProgressHandle::new());
    let (d2, c2, p2, url, dir2) = (plain, control.clone(), progress.clone(), srv.url("/f.bin"), dir.clone());
    let task = tokio::spawn(async move { d2.download(&DownloadRequest::new(url, &dir2), c2, p2).await });
    wait_until(&progress, MB as u64).await;
    control.pause();
    control.cancel(); // a later cancel overrides the pause
    assert!(matches!(task.await.unwrap(), Err(DownloadError::Cancelled)));
}
