//! Phase 13.2: one download engine against every kind of server the roadmap lists: normal, range,
//! no-range, slow, unstable, redirecting, authenticating, throttling (429), failing (500), and
//! servers that lie or misbehave. The server is `velox_integration_tests::serve`, whose behaviour
//! is chosen by the request path (see its module documentation).

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use download_manager::{DownloadManager, ManagerConfig, NewDownload};
use downloader_core::{
    ClientConfig, DownloadConfig, DownloadControl, DownloadError, DownloadRequest, DownloadStatus, Downloader, ProgressHandle, RetryPolicy,
};
use velox_integration_tests::{random_data, serve};

const MB: usize = 1024 * 1024;

fn tmpdir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("velox-srv-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

fn retry(max: u32) -> RetryPolicy {
    RetryPolicy {
        max_retries: max,
        max_throttle_retries: max,
        base_delay: Duration::from_millis(5),
        max_delay: Duration::from_millis(20),
        max_retry_after: Duration::from_millis(50),
    }
}

fn engine(client: ClientConfig, retries: u32) -> Downloader {
    let cfg = DownloadConfig { min_segment_size: 64 * 1024, retry: retry(retries), ..Default::default() };
    Downloader::new(&client, cfg).unwrap()
}

fn plain() -> Downloader {
    engine(ClientConfig::default(), 3)
}

async fn fetch(d: &Downloader, url: String, dir: &PathBuf) -> Result<downloader_core::DownloadOutcome, DownloadError> {
    d.download(&DownloadRequest::new(url, dir), DownloadControl::new(), ProgressHandle::new()).await
}

fn no_final_file(dir: &PathBuf) -> bool {
    !dir.exists() || std::fs::read_dir(dir).unwrap().all(|e| e.unwrap().file_name().to_string_lossy().ends_with(".part"))
}

// ------------------------------------------------------------------ range / no-range

#[tokio::test]
async fn a_range_server_is_downloaded_in_parallel_and_exact() {
    let data = random_data(4 * MB, 101);
    let srv = serve(data.clone()).await;
    let dir = tmpdir("range");
    let o = fetch(&plain(), srv.url("/f.bin"), &dir).await.unwrap();
    assert!(o.parallel, "ranges are supported, so it must use them");
    assert_eq!(std::fs::read(o.path).unwrap(), *data);
}

#[tokio::test]
async fn a_server_without_ranges_is_downloaded_in_one_piece_and_exact() {
    let data = random_data(3 * MB, 102);
    let srv = serve(data.clone()).await;
    let dir = tmpdir("norange");
    let o = fetch(&plain(), srv.url("/norange/f.bin"), &dir).await.unwrap();
    assert!(!o.parallel, "no range support: one connection");
    assert_eq!(o.connections, 1);
    assert_eq!(std::fs::read(o.path).unwrap(), *data);
}

// ------------------------------------------------------------------ redirects

#[tokio::test]
async fn redirect_chains_are_followed_up_to_the_limit_and_loops_are_refused() {
    let data = random_data(MB, 103);
    let srv = serve(data.clone()).await;
    let dir = tmpdir("redir");
    let o = fetch(&plain(), srv.url("/redir/3/f.bin"), &dir).await.unwrap();
    assert_eq!(o.path.file_name().unwrap(), "f.bin", "the name comes from the final address");
    assert_eq!(std::fs::read(&o.path).unwrap(), *data);

    // More hops than the client allows (10): an error, not an endless chase.
    let t = Instant::now();
    let e = fetch(&plain(), srv.url("/redir/40/g.bin"), &tmpdir("redir-long")).await.unwrap_err();
    assert!(t.elapsed() < Duration::from_secs(10), "took {:?}", t.elapsed());
    assert!(!matches!(e, DownloadError::Paused | DownloadError::Cancelled), "{e}");

    // A loop is detected even before the limit.
    let t = Instant::now();
    let e = fetch(&plain(), srv.url("/redirloop"), &tmpdir("redir-loop")).await.unwrap_err();
    assert!(t.elapsed() < Duration::from_secs(10), "took {:?}", t.elapsed());
    assert!(!matches!(e, DownloadError::Paused | DownloadError::Cancelled), "{e}");
}

// ------------------------------------------------------------------ authentication

fn manager_config(name: &str) -> (ManagerConfig, PathBuf) {
    let base = tmpdir(name);
    let mut c = ManagerConfig::new(base.join("state").join("velox.db"), base.join("downloads"));
    c.progress_interval = Duration::from_millis(40);
    c.download.min_segment_size = 64 * 1024;
    c.download.retry = retry(1);
    (c, base.join("downloads"))
}

async fn wait_done(m: &DownloadManager, id: i64) -> download_manager::DownloadDto {
    let t = Instant::now();
    loop {
        let d = m.get_download(id).await.unwrap();
        if matches!(d.status, DownloadStatus::Completed | DownloadStatus::Failed) {
            return d;
        }
        assert!(t.elapsed() < Duration::from_secs(20), "stuck in {:?}", d.status);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn an_authenticating_server_needs_credentials_and_they_are_never_retried_blindly() {
    let data = random_data(MB, 104);
    let srv = serve(data.clone()).await;

    // No credentials: 401, one attempt (retrying a refusal would just lock the account).
    let e = fetch(&plain(), srv.url("/auth/f.bin"), &tmpdir("auth-none")).await.unwrap_err();
    assert!(e.to_string().contains("401"), "{e}");
    let attempts = srv.state.request_heads.lock().unwrap().iter().filter(|h| h.contains("/auth/f.bin")).count();
    assert!(attempts <= 2, "a 401 must not be retried {attempts} times");

    // Credentials in the link: split out, sent as a header, the download completes exactly.
    let (cfg, root) = manager_config("auth-ok");
    let m = DownloadManager::new(cfg).await.unwrap();
    let url = format!("http://user:pass@{}/auth/f.bin", srv.addr);
    let d = m.create_download(NewDownload { url, ..Default::default() }).await.unwrap();
    assert!(!d.url.contains("pass"), "the password must not stay in the link the UI sees: {}", d.url);
    let d = wait_done(&m, d.id).await;
    assert_eq!(d.status, DownloadStatus::Completed, "{:?}", d.error);
    assert_eq!(std::fs::read(root.join("f.bin")).unwrap(), *data);

    // Wrong password: fails, and says so without printing the secret.
    let (cfg, _) = manager_config("auth-bad");
    let m = DownloadManager::new(cfg).await.unwrap();
    let url = format!("http://user:wrong@{}/auth/f.bin", srv.addr);
    let d = m.create_download(NewDownload { url, ..Default::default() }).await.unwrap();
    let d = wait_done(&m, d.id).await;
    assert_eq!(d.status, DownloadStatus::Failed);
    let (msg, detail) = (d.error.unwrap_or_default(), d.error_detail.unwrap_or_default());
    assert!(msg.contains("user name and password") && detail.contains("401"), "{msg} / {detail}");
    assert!(!msg.contains("wrong") && !detail.contains("wrong"), "the password is never printed: {msg} / {detail}");
}

#[tokio::test]
async fn credentials_are_not_forwarded_to_another_origin_by_a_redirect() {
    let data = random_data(MB, 105);
    let a = serve(data.clone()).await; // the server the user trusts
    let b = serve(data.clone()).await; // another origin it redirects to
    *a.state.redirect_to.lock().unwrap() = b.url("/auth/f.bin");

    let (cfg, _) = manager_config("auth-redirect");
    let m = DownloadManager::new(cfg).await.unwrap();
    let url = format!("http://user:pass@{}/redirext", a.addr);
    let d = m.create_download(NewDownload { url, ..Default::default() }).await.unwrap();
    let d = wait_done(&m, d.id).await;

    // B needs the right credentials; it must not have received them from A's redirect.
    assert_eq!(d.status, DownloadStatus::Failed, "the redirect target should have refused an anonymous request");
    let sent = b.state.request_heads.lock().unwrap().iter().any(|h| h.contains("authorization:"));
    assert!(!sent, "the Authorization header followed a redirect to another origin");
}

// ------------------------------------------------------------------ 429 / 500

#[tokio::test]
async fn a_server_that_always_says_429_or_500_ends_in_a_clear_failure_after_bounded_retries() {
    let srv = serve(random_data(MB, 106)).await;
    for (path, code) in [("/e429/f.bin", "429"), ("/e500/f.bin", "500")] {
        let t = Instant::now();
        let e = fetch(&engine(ClientConfig::default(), 3), srv.url(path), &tmpdir(&format!("err{code}"))).await.unwrap_err();
        assert!(e.to_string().contains(code), "{e}");
        assert!(t.elapsed() < Duration::from_secs(10), "{code}: took {:?}", t.elapsed());
        let hits = srv.state.request_heads.lock().unwrap().iter().filter(|h| h.contains(path)).count();
        assert!((2..=12).contains(&hits), "{code}: {hits} requests: retried, but not without end");
    }
}

// ------------------------------------------------------------------ slow / unstable / hostile

#[tokio::test]
async fn a_server_slow_to_answer_is_waited_for_and_can_be_stopped_at_once() {
    let data = random_data(MB, 107);
    let srv = serve(data.clone()).await;
    *srv.state.head_delay.lock().unwrap() = Duration::from_millis(600);
    let o = fetch(&plain(), srv.url("/f.bin"), &tmpdir("slowhead")).await.unwrap();
    assert_eq!(std::fs::read(o.path).unwrap(), *data);

    // Ten seconds of silence, then the user presses Cancel: the engine must not wait them out.
    *srv.state.head_delay.lock().unwrap() = Duration::from_secs(10);
    let control = DownloadControl::new();
    let (c2, dir) = (control.clone(), tmpdir("slowhead-cancel"));
    let url = srv.url("/g.bin");
    let task = tokio::spawn(async move { plain().download(&DownloadRequest::new(url, &dir), c2, ProgressHandle::new()).await });
    tokio::time::sleep(Duration::from_millis(400)).await;
    control.cancel();
    let t = Instant::now();
    let r =
        tokio::time::timeout(Duration::from_secs(3), task).await.expect("cancel was not observed while waiting for the server").unwrap();
    assert!(matches!(r, Err(DownloadError::Cancelled)), "{r:?}");
    assert!(t.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn a_server_preparing_the_file_longer_than_the_check_allows_is_waited_for_once() {
    // A database export: nothing at all until the file is ready, longer than the quick check waits.
    let data = random_data(MB, 111);
    let srv = serve(data.clone()).await;
    *srv.state.head_delay.lock().unwrap() = Duration::from_millis(1500);
    let client = ClientConfig {
        request_timeout: Duration::from_millis(400),
        read_timeout: Duration::from_millis(400),
        slow_answer_wait: Duration::from_secs(20),
        ..Default::default()
    };
    let progress = ProgressHandle::new();
    let o = engine(client, 3)
        .download(&DownloadRequest::new(srv.url("/export.json"), tmpdir("export")), DownloadControl::new(), progress.clone())
        .await
        .unwrap();
    assert_eq!(std::fs::read(&o.path).unwrap(), *data, "saved from the one answer, exact");
    assert!(!o.parallel);
    // The quick check (HEAD) gave up, then one GET waited: the server prepared the file once, no retries.
    let heads = srv.state.request_heads.lock().unwrap().clone();
    let gets: Vec<_> = heads.iter().filter(|h| h.starts_with("get ")).collect();
    assert_eq!(gets.len(), 1, "{heads:?}");
    assert!(!gets[0].contains("range:"), "a plain request, from the start");
    assert!(progress.events().iter().any(|e| matches!(e.kind, downloader_core::RunEventKind::WaitingForServer)));
}

#[tokio::test]
async fn a_file_streamed_without_a_size_is_saved_whole_and_ends_when_the_server_ends_it() {
    let data = random_data(MB + 777, 112);
    let srv = serve(data.clone()).await;
    *srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(5);
    let progress = ProgressHandle::new();
    let o = plain()
        .download(&DownloadRequest::new(srv.url("/chunked/export.json"), tmpdir("chunked")), DownloadControl::new(), progress.clone())
        .await
        .unwrap();
    assert_eq!(std::fs::read(&o.path).unwrap(), *data);
    assert_eq!(o.size, data.len() as u64);

    // The same, from a server that also prepares it longer than the quick check waits.
    *srv.state.head_delay.lock().unwrap() = Duration::from_millis(1200);
    let client = ClientConfig {
        request_timeout: Duration::from_millis(400),
        read_timeout: Duration::from_millis(400),
        slow_answer_wait: Duration::from_secs(20),
        ..Default::default()
    };
    let o = engine(client, 3)
        .download(
            &DownloadRequest::new(srv.url("/chunked/slow.json"), tmpdir("chunked-slow")),
            DownloadControl::new(),
            ProgressHandle::new(),
        )
        .await
        .unwrap();
    assert_eq!(std::fs::read(&o.path).unwrap(), *data);

    // Pauses between two pieces longer than the usual idle limit (the server reads more records meanwhile): a
    // browser waits, so does VeloxFetch, as long as no size was announced.
    *srv.state.head_delay.lock().unwrap() = Duration::ZERO;
    let small = random_data(100 * 1024, 113);
    srv.set_data(small.clone());
    *srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(900);
    let client =
        ClientConfig { request_timeout: Duration::from_millis(400), read_timeout: Duration::from_millis(400), ..Default::default() };
    let o = engine(client, 0)
        .download(
            &DownloadRequest::new(srv.url("/chunked/paused.json"), tmpdir("chunked-paused")),
            DownloadControl::new(),
            ProgressHandle::new(),
        )
        .await
        .unwrap();
    assert_eq!(std::fs::read(&o.path).unwrap(), *small);
}

#[tokio::test]
async fn an_unstable_server_that_resets_or_stalls_fails_cleanly_and_leaves_no_file() {
    let srv = serve(random_data(2 * MB, 108)).await;
    let client = ClientConfig { read_timeout: Duration::from_millis(700), request_timeout: Duration::from_secs(2), ..Default::default() };

    let dir = tmpdir("reset");
    let t = Instant::now();
    let e = fetch(&engine(client.clone(), 2), srv.url("/reset/f.bin"), &dir).await.unwrap_err();
    assert!(t.elapsed() < Duration::from_secs(15), "reset: took {:?}", t.elapsed());
    assert!(!matches!(e, DownloadError::Cancelled), "{e}");
    assert!(no_final_file(&dir));

    // Headers and then silence: the read timeout must fire; nothing hangs for ten minutes.
    let dir = tmpdir("hang");
    let t = Instant::now();
    let e = fetch(&engine(client, 1), srv.url("/hang/f.bin"), &dir).await.unwrap_err();
    assert!(t.elapsed() < Duration::from_secs(20), "hang: took {:?}", t.elapsed());
    assert!(!matches!(e, DownloadError::Cancelled), "{e}");
    assert!(no_final_file(&dir), "a half-received file was published");
}

#[tokio::test]
async fn a_body_shorter_than_its_content_length_is_never_published_as_complete() {
    let data = random_data(MB, 109);
    let srv = serve(data.clone()).await;
    srv.state.lie_extra.store(4096, Ordering::SeqCst); // promises 4 KiB more than it sends
    let dir = tmpdir("lie");
    let e = fetch(&plain(), srv.url("/lie/f.bin"), &dir).await.unwrap_err();
    assert!(!matches!(e, DownloadError::Cancelled), "{e}");
    assert!(no_final_file(&dir), "a truncated file was published");
}

#[tokio::test]
async fn an_absurdly_large_response_header_is_refused_without_harm() {
    let data = random_data(MB, 110);
    let srv = serve(data.clone()).await;
    // A big but plausible header is fine...
    srv.state.junk_header_bytes.store(8 * 1024, Ordering::SeqCst);
    let o = fetch(&plain(), srv.url("/big-header/f.bin"), &tmpdir("hdr-ok")).await.unwrap();
    assert_eq!(std::fs::read(o.path).unwrap(), *data);
    // ...a 4 MiB one is an attack on memory: an error, quickly.
    srv.state.junk_header_bytes.store(4 * MB, Ordering::SeqCst);
    let t = Instant::now();
    let dir = tmpdir("hdr-huge");
    let r = fetch(&plain(), srv.url("/big-header/g.bin"), &dir).await;
    assert!(t.elapsed() < Duration::from_secs(10), "took {:?}", t.elapsed());
    assert!(r.is_err(), "a 4 MiB header should not be accepted");
    assert!(no_final_file(&dir));
}
