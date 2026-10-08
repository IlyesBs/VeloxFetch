//! Download Doctor against real failures: a local server that fails on purpose, a closed port, a name that does not
//! exist, a disk that is "too small", a wrong checksum. Each failure gets the right cause, retry class and actions,
//! and the evidence (retries, the network, a repeat) changes the diagnosis; a fixed problem recovers through Retry;
//! normal downloads carry no diagnosis.

use std::sync::Arc;
use std::time::{Duration, Instant};

use download_manager::doctor::{Action, Category, FactorCode, RetryClass, Severity, Source};
use download_manager::{DownloadDto, DownloadManager, ManagerConfig, NewDownload, SystemHandle};
use downloader_core::{DownloadStatus, RetryPolicy};
use system_integration::{ConnectionKind, DryRunSystem, NetworkState};
use velox_integration_tests::{random_data, serve};

const MB: usize = 1024 * 1024;

async fn manager(name: &str) -> (DownloadManager, Arc<DryRunSystem>) {
    let base = std::env::temp_dir().join(format!("velox-doctor-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let mut c = ManagerConfig::new(base.join("state").join("velox.db"), base.join("downloads"));
    c.download.min_segment_size = 64 * 1024;
    c.download.retry = RetryPolicy {
        max_retries: 2,
        max_throttle_retries: 1,
        base_delay: Duration::from_millis(20),
        max_delay: Duration::from_millis(50),
        ..Default::default()
    };
    c.client.read_timeout = Duration::from_millis(600);
    c.client.request_timeout = Duration::from_millis(600);
    c.client.slow_answer_wait = Duration::from_secs(1);
    let shell = Arc::new(DryRunSystem::new());
    shell.set_network(NetworkState { online: Some(true), kind: Some(ConnectionKind::Wired) });
    c.system = SystemHandle(shell.clone());
    (DownloadManager::new(c).await.unwrap(), shell)
}

async fn settle(m: &DownloadManager, id: i64) -> DownloadDto {
    let t = Instant::now();
    loop {
        let d = m.get_download(id).await.unwrap();
        if matches!(d.status, DownloadStatus::Completed | DownloadStatus::Failed) {
            tokio::time::sleep(Duration::from_millis(100)).await; // the run's diagnostics are written just after
            return m.get_download(id).await.unwrap();
        }
        assert!(t.elapsed() < Duration::from_secs(30), "stuck in {:?}", d.status);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn fail(m: &DownloadManager, req: NewDownload) -> DownloadDto {
    let d = m.create_download(req).await.unwrap();
    let d = settle(m, d.id).await;
    assert_eq!(d.status, DownloadStatus::Failed, "{d:?}");
    d
}

fn url(u: String) -> NewDownload {
    NewDownload { url: u, ..Default::default() }
}

#[tokio::test]
async fn server_errors_are_the_servers_and_wait_before_retrying() {
    let srv = serve(random_data(MB, 1)).await;
    let (m, _) = manager("5xx").await;
    let d = fail(&m, url(srv.url("/e500/a.bin"))).await;
    let x = d.doctor.expect("a diagnosis");
    assert_eq!((x.category, x.source, x.retry), (Category::ServerTemporary, Source::Server, RetryClass::RetryableWithDelay));
    assert_eq!(x.code, "http_500");
    assert_eq!(x.actions[0], Action::RetryLater);
    assert!(x.factors.iter().all(|f| f.code != FactorCode::UnstableConnection), "the network is not blamed");
    // Rate limited: the engine honours Retry-After, then the Doctor says to wait.
    let d = fail(&m, url(srv.url("/e429/b.bin"))).await;
    let x = d.doctor.unwrap();
    assert_eq!((x.category, x.retry), (Category::RateLimited, RetryClass::RetryableWithDelay));
}

#[tokio::test]
async fn sign_in_is_needed_from_the_user() {
    let srv = serve(random_data(MB, 2)).await;
    let (m, _) = manager("auth").await;
    let x = fail(&m, url(srv.url("/auth/a.bin"))).await.doctor.unwrap();
    assert_eq!(
        (x.category, x.source, x.retry, x.severity),
        (Category::AuthRequired, Source::Account, RetryClass::RetryAfterUserAction, Severity::Blocking)
    );
    assert_eq!(x.actions[0], Action::SignIn);
    let x = fail(&m, url(srv.url("/html/report.pdf"))).await.doctor.unwrap();
    assert_eq!(x.category, Category::WebPage);
}

#[tokio::test]
async fn connection_failures_tell_reset_timeout_refused_dns_and_offline_apart() {
    let srv = serve(random_data(MB, 3)).await;
    let (m, shell) = manager("net").await;
    let x = fail(&m, url(srv.url("/reset/a.bin"))).await.doctor.unwrap();
    assert_eq!((x.category, x.source, x.retry), (Category::ConnectionReset, Source::Network, RetryClass::Retryable), "{x:?}");
    let x = fail(&m, url(srv.url("/hang/a.bin"))).await.doctor.unwrap();
    assert!(matches!(x.category, Category::Timeout | Category::ConnectionReset), "{x:?}");
    assert_eq!(x.retry, RetryClass::Retryable);
    // Nothing listens on that port.
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
    let x = fail(&m, url(format!("http://{closed}/a.bin"))).await.doctor.unwrap();
    assert_eq!((x.category, x.retry), (Category::ConnectionFailed, RetryClass::Retryable), "{x:?}");
    // A name that cannot exist (`.invalid` is reserved).
    let x = fail(&m, url("http://no-such-host.invalid/a.bin".into())).await.doctor.unwrap();
    assert_eq!((x.category, x.retry), (Category::Dns, RetryClass::Unknown), "{x:?}");
    // The same failure while the computer reports no network: it is the connection.
    shell.set_network(NetworkState { online: Some(false), kind: None });
    let x = fail(&m, url(format!("http://{closed}/b.bin"))).await.doctor.unwrap();
    assert_eq!((x.category, x.source, x.retry), (Category::Offline, Source::Network, RetryClass::RetryAfterUserAction));
    assert_eq!(x.actions[0], Action::CheckConnection);
}

#[tokio::test]
async fn a_secure_link_to_a_plain_server_is_a_tls_failure_not_retried() {
    // A server that answers in plain text the moment a connection opens: the secure handshake cannot happen.
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let accepted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = accepted.clone();
    tokio::spawn(async move {
        use tokio::io::AsyncWriteExt;
        while let Ok((mut s, _)) = l.accept().await {
            seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let _ = s.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n").await;
        }
    });
    let (m, _) = manager("tls").await;
    let x = fail(&m, url(format!("https://{addr}/a.bin"))).await.doctor.unwrap();
    assert_eq!((x.category, x.retry), (Category::Tls, RetryClass::NotRetryable), "{x:?}");
    assert_eq!(x.source, Source::Server);
    // Not retried by the engine either: the quick check's two requests (HEAD, then the ranged GET), no more.
    let n = accepted.load(std::sync::atomic::Ordering::SeqCst);
    assert!(n <= 2, "a TLS failure that cannot fix itself was retried: {n} connections");
}

#[tokio::test]
async fn a_full_disk_is_this_computers_and_says_how_much() {
    let srv = serve(random_data(MB, 5)).await;
    let (m, _) = manager("disk").await;
    let x = fail(&m, url(srv.url("/huge/big.iso"))).await.doctor.unwrap();
    assert_eq!((x.category, x.source, x.retry), (Category::DiskFull, Source::ThisComputer, RetryClass::RetryAfterUserAction));
    assert_eq!(x.needed_bytes, Some(1 << 50));
    assert!(x.free_bytes.is_some());
    assert_eq!(x.actions[0], Action::FreeSpace);
}

#[tokio::test]
async fn a_checksum_mismatch_is_never_completed_and_a_repeat_stops_retrying() {
    let srv = serve(random_data(MB, 6)).await;
    let (m, _) = manager("sum").await;
    let wrong = format!("sha256:{}", "0".repeat(64));
    let d = fail(&m, NewDownload { url: srv.url("/a.bin"), checksum: Some(wrong), ..Default::default() }).await;
    let x = d.doctor.unwrap();
    assert_eq!((x.category, x.retry, x.severity), (Category::Integrity, RetryClass::Retryable, Severity::Critical));
    assert_eq!(x.actions[0], Action::RestartSafely);
    // Retry downloads it again: the same mismatch, so retrying will not help.
    m.retry_download(d.id).await.unwrap();
    let d = settle(&m, d.id).await;
    assert_eq!(d.status, DownloadStatus::Failed, "a corrupted file is never marked completed");
    let x = d.doctor.unwrap();
    assert_eq!(x.retry, RetryClass::NotRetryable);
    assert!(x.factors.iter().any(|f| f.code == FactorCode::SameAsBefore));
    // The detail view's diagnostics carry the same diagnosis; the timeline names it (no secret).
    let diag = m.get_download_diagnostics(d.id).await.unwrap();
    assert_eq!(diag.doctor.unwrap().category, Category::Integrity);
    assert!(diag.events.iter().any(|e| e.text.starts_with("Doctor: checksum_mismatch")));
}

#[tokio::test]
async fn a_fixed_problem_recovers_through_retry_and_normal_downloads_have_no_diagnosis() {
    let srv = serve(random_data(MB, 7)).await;
    let (m, _) = manager("recover").await;
    let ok = m.create_download(url(srv.url("/fine.bin"))).await.unwrap();
    let ok = settle(&m, ok.id).await;
    assert_eq!(ok.status, DownloadStatus::Completed);
    assert!(ok.doctor.is_none());
    // Offline-style failure first (a closed port), then the server is there: Retry completes, the diagnosis goes.
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    drop(l);
    let d = fail(&m, url(format!("http://{addr}/x.bin"))).await;
    assert!(d.doctor.is_some());
    let srv2 = velox_integration_tests::serve_on(addr, random_data(MB, 8)).await;
    m.retry_download(d.id).await.unwrap();
    let d = settle(&m, d.id).await;
    assert_eq!(d.status, DownloadStatus::Completed);
    assert!(d.doctor.is_none(), "a completed download has nothing to diagnose");
    drop(srv2);
    // Cancelled: no diagnosis either.
    let c = m.create_download(NewDownload { url: srv.url("/hang/c.bin"), ..Default::default() }).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let c = m.cancel_download(c.id).await.unwrap();
    assert!(c.doctor.is_none());
}
