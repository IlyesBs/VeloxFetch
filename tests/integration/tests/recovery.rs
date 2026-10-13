//! Never lose a download: a download that failed because of the network or a server's passing trouble is tried
//! again on its own, from its saved bytes. It waits while the computer is offline (no try spent), is tried at once
//! when the computer wakes up, gives up after its tries, and anything the user does wins (`manager/recovery.rs`).

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use download_manager::{DownloadDto, DownloadManager, ManagerConfig, NewDownload, SystemHandle};
use downloader_core::{DownloadStatus, RetryPolicy};
use system_integration::{ConnectionKind, DryRunSystem, NetworkState};
use velox_integration_tests::{random_data, serve, Server};

const MB: usize = 1024 * 1024;

struct Env {
    root: PathBuf,
    db: PathBuf,
}

impl Drop for Env {
    fn drop(&mut self) {
        if let Some(base) = self.root.parent() {
            let _ = std::fs::remove_dir_all(base);
        }
    }
}

fn env(name: &str) -> Env {
    let base = std::env::temp_dir().join(format!("velox-recovery-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    Env { root: base.join("downloads"), db: base.join("state").join("velox.db") }
}

/// A manager whose own tries come after `delay` (at most `max`), on a computer whose network the test sets.
async fn manager(e: &Env, sys: Arc<DryRunSystem>, delay: Duration, max: u32) -> DownloadManager {
    let mut c = ManagerConfig::new(&e.db, &e.root);
    c.progress_interval = Duration::from_millis(40);
    c.queue_poll = Duration::from_millis(30);
    c.download.min_segment_size = 64 * 1024;
    c.download.checkpoint_interval = Duration::from_millis(50);
    c.download.retry = RetryPolicy {
        max_retries: 1,
        max_throttle_retries: 1,
        base_delay: Duration::from_millis(5),
        max_delay: Duration::from_millis(20),
        max_retry_after: Duration::from_millis(50),
    };
    c.recovery_delays = vec![delay];
    c.recovery_max_tries = max;
    c.system = SystemHandle(sys);
    DownloadManager::new(c).await.unwrap()
}

fn online(sys: &DryRunSystem, on: bool) {
    sys.set_network(NetworkState { online: Some(on), kind: on.then_some(ConnectionKind::Wired) });
}

async fn until(what: &str, secs: u64, mut f: impl AsyncFnMut() -> bool) {
    let t = Instant::now();
    while !f().await {
        assert!(t.elapsed() < Duration::from_secs(secs), "timed out waiting for: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn get(m: &DownloadManager, id: i64) -> DownloadDto {
    m.get_download(id).await.unwrap()
}

/// Runs of `id` that ended failed, from its timeline.
async fn failures(m: &DownloadManager, id: i64) -> usize {
    m.get_download_diagnostics(id).await.unwrap().events.iter().filter(|e| e.text.starts_with("Failed (")).count()
}

async fn diagnostics(m: &DownloadManager, id: i64) -> String {
    format!("{:?}", m.get_download_diagnostics(id).await.unwrap())
}

/// A download of `path` that got going (at least 1 MiB saved), then failed because the server refuses for a while.
async fn failed_part_way(m: &DownloadManager, srv: &Server, path: &str) -> DownloadDto {
    srv.state.fail_next.store(0, Ordering::SeqCst);
    *srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(15);
    let d = m.create_download(NewDownload { url: srv.url(path), ..Default::default() }).await.unwrap();
    until("1 MiB saved", 20, async || get(m, d.id).await.downloaded >= MB as u64).await;
    srv.state.fail_next.store(1_000_000, Ordering::SeqCst);
    *srv.state.chunk_delay.lock().unwrap() = Duration::ZERO;
    // The running parts end; the next requests are refused: the run fails.
    srv.state.truncate_next.store(64, Ordering::SeqCst);
    // The engine saves "failed" a moment before the manager plans the next try.
    until("failed and waiting", 20, async || {
        let x = get(m, d.id).await;
        x.status == DownloadStatus::Failed && x.auto_retry.is_some()
    })
    .await;
    srv.state.truncate_next.store(0, Ordering::SeqCst);
    get(m, d.id).await
}

#[tokio::test]
async fn a_server_in_trouble_is_tried_again_on_its_own_and_the_download_finishes() {
    let data = random_data(6 * MB, 201);
    let srv = serve(data.clone()).await;
    let sys = Arc::new(DryRunSystem::new());
    online(&sys, true);
    let e = env("server");
    let m = manager(&e, sys, Duration::from_millis(300), 20).await;
    let failed = failed_part_way(&m, &srv, "/a.bin").await;
    let a = failed.auto_retry.clone().expect("tried again on its own");
    assert!(!a.waiting_for_network && a.at_ms.is_some() && a.tries == 0, "{a:?}");
    assert!(failed.doctor.as_ref().and_then(|d| d.retry_at_ms).is_some(), "the Doctor says when");
    // Still refusing: a few tries are spent, the bytes stay.
    until("2 tries", 20, async || get(&m, failed.id).await.auto_retry.is_some_and(|a| a.tries >= 2)).await;
    assert!(get(&m, failed.id).await.downloaded >= failed.downloaded, "saved bytes kept between tries");
    // The server is fine again: the next try finishes from the saved bytes.
    let before = srv.state.bytes_served.load(Ordering::SeqCst);
    srv.state.fail_next.store(0, Ordering::SeqCst);
    until("completed", 30, async || get(&m, failed.id).await.status == DownloadStatus::Completed).await;
    assert!(std::fs::read(e.root.join("a.bin")).unwrap() == *data);
    let served = srv.state.bytes_served.load(Ordering::SeqCst) - before;
    assert!(served <= (data.len() as u64 - failed.downloaded) + MB as u64, "continued, not started over: {served}");
    assert!(get(&m, failed.id).await.auto_retry.is_none());
    let diag = diagnostics(&m, failed.id).await;
    assert!(diag.contains("Trying again on its own (try 1 of 20)"), "{diag}");
    assert!(!diag.contains("starting over"), "{diag}");
}

#[tokio::test]
async fn offline_it_waits_without_spending_tries_and_continues_when_the_network_is_back() {
    let data = random_data(6 * MB, 202);
    let srv = serve(data.clone()).await;
    let sys = Arc::new(DryRunSystem::new());
    online(&sys, true);
    let e = env("offline");
    let m = manager(&e, sys.clone(), Duration::from_millis(200), 3).await;
    let failed = failed_part_way(&m, &srv, "/b.bin").await;
    online(&sys, false);
    until("waiting for the network", 20, async || get(&m, failed.id).await.auto_retry.is_some_and(|a| a.waiting_for_network)).await;
    // A try may have started in the moment before "offline" was seen: from now on, none.
    let tries = get(&m, failed.id).await.auto_retry.unwrap().tries;
    let hits = srv.state.hits.load(Ordering::SeqCst);
    // Far longer than every try would take online (3 × 200 ms): offline, none is spent and nothing is asked.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let d = get(&m, failed.id).await;
    assert_eq!(d.status, DownloadStatus::Failed);
    assert_eq!(d.auto_retry.as_ref().map(|a| (a.tries, a.at_ms)), Some((tries, None)), "no try spent offline");
    assert_eq!(srv.state.hits.load(Ordering::SeqCst), hits, "nothing asked while offline");
    // Back online (and the server too): it continues on its own.
    srv.state.fail_next.store(0, Ordering::SeqCst);
    online(&sys, true);
    until("completed", 30, async || get(&m, failed.id).await.status == DownloadStatus::Completed).await;
    assert!(std::fs::read(e.root.join("b.bin")).unwrap() == *data);
}

#[tokio::test]
async fn after_the_computer_wakes_up_a_waiting_download_is_tried_at_once() {
    let data = random_data(4 * MB, 203);
    let srv = serve(data.clone()).await;
    let sys = Arc::new(DryRunSystem::new());
    online(&sys, true);
    let e = env("wake");
    // The next try would be in an hour.
    let m = manager(&e, sys, Duration::from_secs(3600), 20).await;
    let failed = failed_part_way(&m, &srv, "/c.bin").await;
    srv.state.fail_next.store(0, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(get(&m, failed.id).await.status, DownloadStatus::Failed, "waits its turn");
    m.system_resumed();
    until("completed", 30, async || get(&m, failed.id).await.status == DownloadStatus::Completed).await;
    assert!(std::fs::read(e.root.join("c.bin")).unwrap() == *data);
}

#[tokio::test]
async fn it_gives_up_after_its_tries_and_the_doctor_explains() {
    let data = random_data(4 * MB, 204);
    let srv = serve(data.clone()).await;
    let sys = Arc::new(DryRunSystem::new());
    online(&sys, true);
    let e = env("giveup");
    let m = manager(&e, sys, Duration::from_millis(100), 2).await;
    let failed = failed_part_way(&m, &srv, "/d.bin").await;
    until("given up", 30, async || {
        let d = get(&m, failed.id).await;
        // The first run and both tries failed (each run ends with its own "Failed (...)" line).
        d.status == DownloadStatus::Failed && d.auto_retry.is_none() && failures(&m, failed.id).await >= 3
    })
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(get(&m, failed.id).await.status, DownloadStatus::Failed, "no third try");
    let d = get(&m, failed.id).await;
    assert!(d.doctor.as_ref().is_some_and(|x| x.retry_at_ms.is_none()), "no next try announced");
    assert!(d.downloaded >= failed.downloaded, "the bytes stay for the user's Retry");
    let diag = diagnostics(&m, failed.id).await;
    assert!(diag.contains("try 2 of 2") && !diag.contains("try 3 of"), "{diag}");
    // The user's Retry counts anew and finishes.
    srv.state.fail_next.store(0, Ordering::SeqCst);
    m.retry_download(failed.id).await.unwrap();
    until("completed", 30, async || get(&m, failed.id).await.status == DownloadStatus::Completed).await;
    assert!(std::fs::read(e.root.join("d.bin")).unwrap() == *data);
}

#[tokio::test]
async fn what_needs_the_user_is_never_tried_on_its_own() {
    let srv = serve(random_data(MB, 205)).await;
    let sys = Arc::new(DryRunSystem::new());
    online(&sys, true);
    let e = env("needsuser");
    let m = manager(&e, sys, Duration::from_millis(50), 20).await;
    srv.state.expired.lock().unwrap().insert("tok".into());
    // A link that expired (403: sign in or a new link) and a file that is not there (404).
    for path in ["/signed/tok/e.bin", "/404/e.bin"] {
        let d = m.create_download(NewDownload { url: srv.url(path), ..Default::default() }).await.unwrap();
        until("failed", 20, async || get(&m, d.id).await.status == DownloadStatus::Failed).await;
        let hits = srv.state.hits.load(Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(500)).await;
        let now = get(&m, d.id).await;
        assert!(now.auto_retry.is_none() && now.status == DownloadStatus::Failed, "{path}: {:?}", now.error_code);
        assert_eq!(srv.state.hits.load(Ordering::SeqCst), hits, "{path}: not asked again");
    }
}

#[tokio::test]
async fn pause_stops_the_tries_and_keeps_the_bytes() {
    let data = random_data(6 * MB, 206);
    let srv = serve(data.clone()).await;
    let sys = Arc::new(DryRunSystem::new());
    online(&sys, true);
    let e = env("pause");
    let m = manager(&e, sys, Duration::from_millis(400), 20).await;
    let failed = failed_part_way(&m, &srv, "/g.bin").await;
    assert!(failed.auto_retry.is_some());
    let paused = m.pause_download(failed.id).await.unwrap();
    assert_eq!(paused.status, DownloadStatus::Paused);
    assert!(paused.auto_retry.is_none());
    let hits = srv.state.hits.load(Ordering::SeqCst);
    srv.state.fail_next.store(0, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert_eq!(get(&m, failed.id).await.status, DownloadStatus::Paused, "never started on its own once paused");
    assert_eq!(srv.state.hits.load(Ordering::SeqCst), hits);
    m.resume_download(failed.id).await.unwrap();
    until("completed", 30, async || get(&m, failed.id).await.status == DownloadStatus::Completed).await;
    assert!(std::fs::read(e.root.join("g.bin")).unwrap() == *data);
    // A failed download that is not waiting still cannot be paused.
    let srv2 = serve(random_data(MB, 207)).await;
    let d = m.create_download(NewDownload { url: srv2.url("/404/h.bin"), ..Default::default() }).await.unwrap();
    until("failed", 20, async || get(&m, d.id).await.status == DownloadStatus::Failed).await;
    assert!(m.pause_download(d.id).await.is_err());
}

#[tokio::test]
async fn cancel_and_remove_forget_the_tries() {
    let data = random_data(6 * MB, 208);
    let srv = serve(data.clone()).await;
    let sys = Arc::new(DryRunSystem::new());
    online(&sys, true);
    let e = env("forget");
    let m = manager(&e, sys, Duration::from_millis(300), 20).await;
    let a = failed_part_way(&m, &srv, "/i.bin").await;
    m.cancel_download(a.id).await.unwrap();
    let b = failed_part_way(&m, &srv, "/j.bin").await;
    m.delete_download(b.id, false).await.unwrap();
    let hits = srv.state.hits.load(Ordering::SeqCst);
    srv.state.fail_next.store(0, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert_eq!(get(&m, a.id).await.status, DownloadStatus::Cancelled);
    assert!(m.get_download(b.id).await.is_err());
    assert_eq!(srv.state.hits.load(Ordering::SeqCst), hits, "nothing tried after cancel or remove");
}
