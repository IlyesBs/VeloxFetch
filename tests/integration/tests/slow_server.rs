//! Issue #21: a slow server must not download slower in VeloxFetch than in a browser.
//!
//! A browser keeps one unbroken connection. VeloxFetch lost time three ways, each covered here:
//! * lowering its connection count cut a connection that was transferring (the bytes on their way and the wait for a
//!   new answer lost), while the requests still waiting for an answer stayed;
//! * joins between parts that started together were checked at the very end, one extra request after the other;
//! * a single-connection transfer (a server slow to answer) started again from byte 0 after every broken connection.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::Duration;

use downloader_core::{
    AdaptiveConfig, ClientConfig, DownloadConfig, DownloadControl, DownloadRequest, Downloader, ProgressHandle, RetryPolicy,
};
use velox_integration_tests::{random_data, serve};

const MB: usize = 1024 * 1024;

fn tmpdir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("velox-slow-{}-{name}", std::process::id()));
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

/// A server that prepares the file longer than the quick check waits (see `HttpError::SlowToAnswer`).
fn slow_to_answer() -> ClientConfig {
    ClientConfig {
        request_timeout: Duration::from_millis(400),
        read_timeout: Duration::from_millis(400),
        slow_answer_wait: Duration::from_secs(20),
        ..Default::default()
    }
}

fn ranges(heads: &[String]) -> Vec<(u64, u64)> {
    heads
        .iter()
        .filter(|h| h.starts_with("get "))
        .filter_map(|h| {
            let r = h.lines().find_map(|l| l.strip_prefix("range: bytes="))?;
            let (a, b) = r.trim().split_once('-')?;
            Some((a.parse().ok()?, b.parse().unwrap_or(u64::MAX)))
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fewer_connections_never_cut_a_transfer_only_requests_still_waiting() {
    // One request served at a time: extra connections bring nothing, so the download's own judgement lowers the
    // count again. The one transferring must go on; the ones waiting for an answer are the ones to stop.
    let data = random_data(6 * MB, 2101);
    let srv = serve(data.clone()).await;
    srv.state.one_at_a_time.store(true, Ordering::SeqCst);
    *srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(40);
    let client = ClientConfig { request_timeout: Duration::from_secs(60), ..Default::default() };
    // Every step up is judged as bringing nothing (as it really does here), so the count goes back down whatever
    // the machine's load does to the measured speed.
    let adaptive = AdaptiveConfig { min_gain: 100.0, ..Default::default() };
    let cfg = DownloadConfig { min_segment_size: 64 * 1024, retry: retry(3), adaptive, ..Default::default() };
    let progress = ProgressHandle::new();
    let o = Downloader::new(&client, cfg)
        .unwrap()
        .download(&DownloadRequest::new(srv.url("/f.bin"), tmpdir("one-at-a-time")), DownloadControl::new(), progress.clone())
        .await
        .unwrap();
    assert_eq!(std::fs::read(&o.path).unwrap(), *data);
    assert!(o.parallel);
    let cut = srv.state.cut_after_chunks.lock().unwrap().clone();
    assert!(!cut.is_empty(), "the count went down while requests waited (the case under test)");
    // A request given up while it waited for its turn is sent its first chunk or two before the server notices.
    assert!(cut.iter().all(|c| *c < 3), "a transfer in progress was cut (chunks sent before): {cut:?}");
    assert_eq!(progress.snapshot().segments.iter().map(|s| s.retry_count).sum::<u32>(), 0, "no failure either");
}

#[tokio::test]
async fn parts_that_start_together_check_their_joins_without_extra_requests_at_the_end() {
    let data = random_data(4 * MB, 2102);
    let srv = serve(data.clone()).await;
    srv.state.keep_alive.store(true, Ordering::SeqCst);
    *srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(3);
    let cfg = DownloadConfig {
        min_segment_size: 64 * 1024,
        max_connections: 16,
        retry: retry(3),
        adaptive: AdaptiveConfig { enabled: false, ..Default::default() },
        ..Default::default()
    };
    let o = Downloader::new(&ClientConfig::default(), cfg)
        .unwrap()
        .download(&DownloadRequest::new(srv.url("/f.bin"), tmpdir("joins")), DownloadControl::new(), ProgressHandle::new())
        .await
        .unwrap();
    assert_eq!(std::fs::read(&o.path).unwrap(), *data);
    let heads = srv.state.request_heads.lock().unwrap().clone();
    let all = ranges(&heads);
    assert!(all.iter().filter(|(a, _)| *a > 0).count() >= 8, "parts started side by side (the case under test): {all:?}");
    // A final look is a 2 KiB window around a join; every join was checked from the requests themselves.
    let looks: Vec<_> = all.iter().filter(|(a, b)| b - a + 1 == 2048).collect();
    assert!(looks.is_empty(), "joins fetched again at the end: {looks:?}");
}

#[tokio::test]
async fn a_slow_servers_broken_answer_continues_where_it_stopped() {
    let data = random_data(MB, 2103);
    let srv = serve(data.clone()).await;
    *srv.state.head_delay.lock().unwrap() = Duration::from_millis(1200);
    srv.state.truncate_next.store(1, Ordering::SeqCst); // the first answer breaks halfway
    let cfg = DownloadConfig { retry: retry(3), ..Default::default() };
    let o = Downloader::new(&slow_to_answer(), cfg)
        .unwrap()
        .download(&DownloadRequest::new(srv.url("/export.bin"), tmpdir("continue")), DownloadControl::new(), ProgressHandle::new())
        .await
        .unwrap();
    assert_eq!(std::fs::read(&o.path).unwrap(), *data, "exact");
    assert!(!o.parallel);
    let heads = srv.state.request_heads.lock().unwrap().clone();
    let gets: Vec<_> = heads.iter().filter(|h| h.starts_with("get ")).collect();
    assert_eq!(gets.len(), 2, "the answer, then the rest of it: {heads:?}");
    let rest = ranges(&heads);
    let half = (data.len() / 2) as u64;
    assert_eq!(rest.len(), 1, "{rest:?}");
    assert!(rest[0].0 > 0 && rest[0].0 <= half && rest[0].0 + 1024 >= half, "from the saved bytes (less 1 KiB checked again): {rest:?}");
    assert!(gets[1].contains("if-range: \"v1\""), "only the same version may continue it: {}", gets[1]);
    let served = srv.state.bytes_served.load(Ordering::SeqCst);
    assert!(served < (data.len() + data.len() / 10) as u64, "not from byte 0 again: {served} bytes sent");
}

#[tokio::test]
async fn a_slow_server_that_cannot_continue_starts_again_from_zero_and_is_not_asked_again() {
    let data = random_data(MB, 2104);
    let srv = serve(data.clone()).await;
    *srv.state.head_delay.lock().unwrap() = Duration::from_millis(1200);
    // Three answers break halfway: the first one, the "rest" (a whole file instead: refused), the first restart.
    srv.state.truncate_next.store(3, Ordering::SeqCst);
    let cfg = DownloadConfig { retry: retry(5), ..Default::default() };
    let o = Downloader::new(&slow_to_answer(), cfg)
        .unwrap()
        .download(&DownloadRequest::new(srv.url("/norange/export.bin"), tmpdir("norange")), DownloadControl::new(), ProgressHandle::new())
        .await
        .unwrap();
    assert_eq!(std::fs::read(&o.path).unwrap(), *data, "exact, as before");
    let heads = srv.state.request_heads.lock().unwrap().clone();
    assert_eq!(ranges(&heads).len(), 1, "asked once for the rest, never again after it was refused: {heads:?}");
    assert_eq!(heads.iter().filter(|h| h.starts_with("get ")).count(), 4, "{heads:?}");
}

#[tokio::test]
async fn a_file_changed_on_the_server_after_the_break_is_never_joined_to_the_saved_part() {
    let old = random_data(MB, 2105);
    let new = random_data(MB, 2106); // same size, another version
    let srv = serve(old.clone()).await;
    *srv.state.head_delay.lock().unwrap() = Duration::from_millis(1200);
    *srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(40);
    srv.state.truncate_next.store(1, Ordering::SeqCst);
    let cfg = DownloadConfig { retry: retry(3), ..Default::default() };
    let progress = ProgressHandle::new();
    let (d, p2, dir) = (Downloader::new(&slow_to_answer(), cfg).unwrap(), progress.clone(), tmpdir("changed"));
    let url = srv.url("/export.bin");
    let task = tokio::spawn(async move { d.download(&DownloadRequest::new(url, &dir), DownloadControl::new(), p2).await });
    // While the first answer comes (old version), the file changes: the next request gets the new one.
    while progress.snapshot().downloaded == 0 {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    srv.set_data(new.clone());
    *srv.state.etag.lock().unwrap() = "\"v2\"".into();
    let o = task.await.unwrap().unwrap();
    assert_eq!(std::fs::read(&o.path).unwrap(), *new, "the new version whole, never half of each");
}
