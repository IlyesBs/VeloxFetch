//! "Use a new link": a stopped download continued from a fresh link (an expired signed link, a mirror, a renewed
//! browser session). Saved bytes are only reused when the new link is proven to give the same file; anything else
//! changes nothing and keeps them.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use download_manager::relink::{Mode, Reason, UseNewLink, Verdict};
use download_manager::{BrowserContext, DownloadDto, DownloadManager, ManagerConfig, ManagerError, NewDownload};
use downloader_core::{AdaptiveConfig, DownloadStatus, RetryPolicy};
use velox_integration_tests::{random_data, serve, Server};

const MB: usize = 1024 * 1024;
const SAMPLES: u64 = 2 * 64 * 1024;

struct Env {
    root: PathBuf,
    db: PathBuf,
}

fn env(name: &str) -> Env {
    let base = std::env::temp_dir().join(format!("velox-relink-{}-{name}", std::process::id()));
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

async fn wait_status(m: &DownloadManager, id: i64, want: DownloadStatus) -> DownloadDto {
    let t = Instant::now();
    loop {
        let d = m.get_download(id).await.unwrap();
        if d.status == want {
            return d;
        }
        assert!(t.elapsed() < Duration::from_secs(30), "download {id} stuck in {:?}, wanted {want:?} ({:?})", d.status, d.error);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A download of `path` stopped part-way: paused, its signed link then expired, and a resume refused with 403.
async fn expired_part_way(m: &DownloadManager, srv: &Server, token: &str, req: NewDownload) -> DownloadDto {
    *srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(15);
    let d = m.create_download(req).await.unwrap();
    let t = Instant::now();
    while m.get_download(d.id).await.unwrap().downloaded < MB as u64 {
        assert!(t.elapsed() < Duration::from_secs(20), "never got going");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    m.pause_download(d.id).await.unwrap();
    wait_status(m, d.id, DownloadStatus::Paused).await;
    *srv.state.chunk_delay.lock().unwrap() = Duration::ZERO;
    srv.state.expired.lock().unwrap().insert(token.to_string());
    m.resume_download(d.id).await.unwrap();
    let failed = wait_status(m, d.id, DownloadStatus::Failed).await;
    assert!(failed.downloaded >= MB as u64, "an expired link keeps the saved bytes: {}", failed.downloaded);
    failed
}

fn signed(srv: &Server, token: &str) -> NewDownload {
    NewDownload { url: srv.url(&format!("/signed/{token}/disc.iso")), ..Default::default() }
}

fn link(id: i64, url: String) -> UseNewLink {
    UseNewLink { id, url: Some(url), ..Default::default() }
}

fn part(e: &Env) -> PathBuf {
    e.root.join("disc.iso.part")
}

/// Requests a server got for this path since `from`.
fn hits(srv: &Server, needle: &str, from: usize) -> usize {
    srv.state.request_heads.lock().unwrap()[from..].iter().filter(|h| h.contains(needle)).count()
}

#[tokio::test]
async fn an_expired_link_continues_from_a_fresh_one_without_starting_over() {
    let data = random_data(6 * MB, 71);
    let srv = serve(data.clone()).await;
    let e = env("expired");
    let m = manager(&e).await;
    let failed = expired_part_way(&m, &srv, "old", signed(&srv, "old")).await;
    let kept = failed.downloaded;

    let (served, heads) = (srv.state.bytes_served.load(Ordering::SeqCst), srv.state.request_heads.lock().unwrap().len());
    let o = m.use_new_link(link(failed.id, srv.url("/signed/new/disc.iso?X-Amz-Signature=TOPSECRET123"))).await.unwrap();
    assert_eq!((o.verdict, o.reason, o.applied), (Verdict::VerifiedSame, Reason::EtagMatch, true));
    assert!(o.kept_bytes >= MB as u64);
    let done = wait_status(&m, failed.id, DownloadStatus::Completed).await;
    assert_eq!(std::fs::read(e.root.join("disc.iso")).unwrap(), *data, "byte for byte the file");
    assert!(done.url.contains("/signed/new/"), "the record follows the new link");
    let fetched = srv.state.bytes_served.load(Ordering::SeqCst) - served;
    assert!(fetched <= data.len() as u64 - kept + SAMPLES + 2 * MB as u64 / 2, "only the missing parts: {fetched} of {}", data.len());
    assert!(fetched < data.len() as u64, "never from zero");
    assert_eq!(hits(&srv, "/signed/old/", heads), 0, "the expired link is not asked again");
    let diag = m.get_download_diagnostics(failed.id).await.unwrap();
    let text = format!("{diag:?}");
    assert!(text.contains("New link: the same file"), "noted in the diagnostics");
    assert!(!text.contains("TOPSECRET123"), "the new link's secret is never written there");
}

#[tokio::test]
async fn a_mirror_elsewhere_is_used_but_never_gets_the_sites_cookies() {
    let data = random_data(4 * MB, 72);
    let (site, mirror) = (serve(data.clone()).await, serve(data.clone()).await);
    let e = env("mirror");
    let m = manager(&e).await;
    let ctx = BrowserContext { cookies: Some("sid=secret".into()), ..Default::default() };
    *site.state.chunk_delay.lock().unwrap() = Duration::from_millis(15);
    let d = m.create_download_from_browser(signed(&site, "s1"), ctx).await.unwrap();
    while m.get_download(d.id).await.unwrap().downloaded < MB as u64 {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    m.pause_download(d.id).await.unwrap();
    wait_status(&m, d.id, DownloadStatus::Paused).await;
    assert!(site.state.request_heads.lock().unwrap().iter().any(|h| h.contains("cookie: sid=secret")));

    let o = m.use_new_link(link(d.id, mirror.url("/pub/disc.iso"))).await.unwrap();
    assert_eq!((o.verdict, o.applied), (Verdict::VerifiedSame, true), "same strong ETag and size, bytes match");
    wait_status(&m, d.id, DownloadStatus::Completed).await;
    assert_eq!(std::fs::read(e.root.join("disc.iso")).unwrap(), *data);
    assert!(!mirror.state.request_heads.lock().unwrap().iter().any(|h| h.contains("cookie:")), "no cookie crosses to another server");
}

#[tokio::test]
async fn another_file_under_the_same_name_is_refused_and_progress_kept() {
    let data = random_data(4 * MB, 73);
    let srv = serve(data.clone()).await;
    let e = env("other");
    let m = manager(&e).await;
    let failed = expired_part_way(&m, &srv, "a", signed(&srv, "a")).await;
    let part_before = std::fs::read(part(&e)).unwrap();
    let check = |o: download_manager::relink::NewLinkOutcome, reason| {
        assert_eq!((o.verdict, o.reason, o.applied, o.can_confirm), (Verdict::DifferentResource, reason, false, false));
        assert_eq!(o.download.unwrap().downloaded, failed.downloaded, "nothing saved is lost");
    };

    // Another size.
    let bigger = serve(random_data(5 * MB, 73)).await;
    check(m.use_new_link(link(failed.id, bigger.url("/disc.iso"))).await.unwrap(), Reason::SizeChanged);
    // Same name, same size, same ETag — other bytes.
    let impostor = serve(random_data(4 * MB, 999)).await;
    check(m.use_new_link(link(failed.id, impostor.url("/disc.iso"))).await.unwrap(), Reason::BytesDiffer);
    // Same bytes, but the server now tags another version.
    let retagged = serve(data.clone()).await;
    *retagged.state.etag.lock().unwrap() = "\"v2\"".into();
    check(m.use_new_link(link(failed.id, retagged.url("/disc.iso"))).await.unwrap(), Reason::EtagChanged);
    // A sign-in page where the file was.
    let page = serve(data.clone()).await;
    check(m.use_new_link(link(failed.id, page.url("/html/disc.iso"))).await.unwrap(), Reason::WebPage);

    let after = m.get_download(failed.id).await.unwrap();
    assert_eq!((after.status, after.downloaded), (DownloadStatus::Failed, failed.downloaded));
    assert!(after.url.contains("/signed/a/"), "the record keeps its link");
    assert_eq!(std::fs::read(part(&e)).unwrap(), part_before, "the partial file is untouched");
}

#[tokio::test]
async fn a_link_that_cannot_continue_or_prove_the_file_changes_nothing() {
    let data = random_data(4 * MB, 74);
    let srv = serve(data.clone()).await;
    let e = env("cannot");
    let m = manager(&e).await;
    let failed = expired_part_way(&m, &srv, "a", signed(&srv, "a")).await;

    // The same file, but the link no longer serves ranges: nothing is appended blindly.
    let o = m.use_new_link(link(failed.id, srv.url("/norange/disc.iso"))).await.unwrap();
    assert_eq!((o.reason, o.applied, o.can_confirm), (Reason::NoRanges, false, false));
    // The same bytes without any validator: a change mid-download could not be noticed.
    let o = m.use_new_link(link(failed.id, srv.url("/noetag/disc.iso"))).await.unwrap();
    assert_eq!((o.verdict, o.reason, o.applied, o.can_confirm), (Verdict::LikelySame, Reason::NoValidator, false, false));
    // The very link it already has: asking again would fail the same way.
    let o = m.use_new_link(link(failed.id, srv.url("/signed/a/disc.iso#again"))).await.unwrap();
    assert_eq!((o.reason, o.applied), (Reason::SameSource, false));
    // A link that refuses.
    let o = m.use_new_link(link(failed.id, srv.url("/404/disc.iso"))).await.unwrap();
    assert_eq!((o.reason, o.http_status, o.applied), (Reason::Unreachable, Some(404), false));
    // A link that expired too.
    srv.state.expired.lock().unwrap().insert("b".into());
    let o = m.use_new_link(link(failed.id, srv.url("/signed/b/disc.iso"))).await.unwrap();
    assert_eq!((o.reason, o.http_status, o.applied), (Reason::Unreachable, Some(403), false));

    let after = m.get_download(failed.id).await.unwrap();
    assert_eq!((after.status, after.downloaded), (DownloadStatus::Failed, failed.downloaded));
}

#[tokio::test]
async fn likely_the_same_file_waits_for_the_users_word_then_completes() {
    let data = random_data(4 * MB, 75);
    let srv = serve(data.clone()).await;
    let e = env("likely");
    let m = manager(&e).await;
    let failed = expired_part_way(&m, &srv, "a", signed(&srv, "a")).await;
    // No ETag now, but a date, and the saved bytes match: likely, not proven.
    let url = srv.url("/noetag/dated/disc.iso");
    let o = m.use_new_link(link(failed.id, url.clone())).await.unwrap();
    assert_eq!((o.verdict, o.reason, o.applied, o.can_confirm), (Verdict::LikelySame, Reason::BytesMatch, false, true));
    assert_eq!(m.get_download(failed.id).await.unwrap().status, DownloadStatus::Failed);

    let o = m.use_new_link(UseNewLink { mode: Mode::Confirm, ..link(failed.id, url) }).await.unwrap();
    assert!(o.applied);
    wait_status(&m, failed.id, DownloadStatus::Completed).await;
    assert_eq!(std::fs::read(e.root.join("disc.iso")).unwrap(), *data);
}

#[tokio::test]
async fn the_browsers_fresh_session_for_a_waiting_link_continues_the_download() {
    let data = random_data(4 * MB, 76);
    let srv = serve(data.clone()).await;
    let e = env("pending");
    let m = manager(&e).await;
    let failed = expired_part_way(&m, &srv, "a", signed(&srv, "a")).await;
    let ctx = BrowserContext { cookies: Some("sid=fresh".into()), ..Default::default() };
    let p = m.add_pending(signed(&srv, "b"), ctx).await.unwrap();
    let heads = srv.state.request_heads.lock().unwrap().len();
    let o = m.use_new_link(UseNewLink { id: failed.id, pending: Some(p.token), ..Default::default() }).await.unwrap();
    assert!(o.applied);
    assert!(m.list_pending().is_empty(), "answered: no second download is added");
    wait_status(&m, failed.id, DownloadStatus::Completed).await;
    assert_eq!(std::fs::read(e.root.join("disc.iso")).unwrap(), *data);
    assert!(hits(&srv, "cookie: sid=fresh", heads) > 0, "the browser's session goes with its link");
    assert_eq!(m.list_downloads(Default::default()).await.unwrap().len(), 1);
}

#[tokio::test]
async fn only_a_stopped_download_and_one_check_at_a_time() {
    let data = random_data(4 * MB, 77);
    let srv = serve(data.clone()).await;
    let e = env("states");
    let m = manager(&e).await;
    *srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(20);
    let running = m.create_download(signed(&srv, "r")).await.unwrap();
    wait_status(&m, running.id, DownloadStatus::Downloading).await;
    let r = m.use_new_link(link(running.id, srv.url("/signed/r2/disc.iso"))).await;
    assert!(matches!(r, Err(ManagerError::InvalidState { .. })), "a running download is left alone");
    m.pause_download(running.id).await.unwrap();
    wait_status(&m, running.id, DownloadStatus::Paused).await;
    assert!(m.use_new_link(UseNewLink { id: running.id, ..Default::default() }).await.is_err(), "a link or a waiting download is needed");
    assert!(m.use_new_link(link(running.id, "ftp://x/y".into())).await.is_err(), "only web links");
    assert!(m.use_new_link(link(999_999, srv.url("/x"))).await.is_err(), "unknown id");

    // Two checks at once for the same download: the second is refused, the first decides.
    *srv.state.chunk_delay.lock().unwrap() = Duration::ZERO;
    let slow = serve(data.clone()).await;
    *slow.state.head_delay.lock().unwrap() = Duration::from_millis(400);
    let (m1, m2, u) = (m.clone(), m.clone(), slow.url("/disc.iso"));
    let (a, b) = tokio::join!(m1.use_new_link(link(running.id, u.clone())), async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        m2.use_new_link(link(running.id, u)).await
    });
    assert!(a.unwrap().applied);
    assert!(matches!(b, Err(ManagerError::InvalidInput(_))), "{b:?}");
    wait_status(&m, running.id, DownloadStatus::Completed).await;
    assert_eq!(std::fs::read(e.root.join("disc.iso")).unwrap(), *data);
}

#[tokio::test]
async fn cancelling_during_the_check_wins_and_nothing_is_switched() {
    let data = random_data(4 * MB, 78);
    let srv = serve(data.clone()).await;
    let e = env("cancel");
    let m = manager(&e).await;
    let failed = expired_part_way(&m, &srv, "a", signed(&srv, "a")).await;
    let slow = serve(data.clone()).await;
    *slow.state.head_delay.lock().unwrap() = Duration::from_millis(500);
    let (m2, u) = (m.clone(), slow.url("/disc.iso"));
    let check = tokio::spawn(async move { m2.use_new_link(link(failed.id, u)).await });
    tokio::time::sleep(Duration::from_millis(150)).await;
    m.cancel_download(failed.id).await.unwrap();
    assert!(check.await.unwrap().is_err(), "the download changed meanwhile: not switched");
    let d = m.get_download(failed.id).await.unwrap();
    assert_eq!(d.status, DownloadStatus::Cancelled);
    assert!(d.url.contains("/signed/a/"));
}

#[tokio::test]
async fn a_restart_after_the_switch_continues_from_the_new_link() {
    let data = random_data(6 * MB, 79);
    let srv = serve(data.clone()).await;
    let e = env("restart");
    let id = {
        let m = manager(&e).await;
        let failed = expired_part_way(&m, &srv, "a", signed(&srv, "a")).await;
        *srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(15);
        let o = m.use_new_link(link(failed.id, srv.url("/signed/b/disc.iso"))).await.unwrap();
        assert!(o.applied);
        m.pause_download(failed.id).await.unwrap();
        wait_status(&m, failed.id, DownloadStatus::Paused).await;
        m.shutdown().await;
        failed.id
    };
    *srv.state.chunk_delay.lock().unwrap() = Duration::ZERO;
    let heads = srv.state.request_heads.lock().unwrap().len();
    let m = manager(&e).await;
    m.resume_download(id).await.unwrap();
    wait_status(&m, id, DownloadStatus::Completed).await;
    assert_eq!(std::fs::read(e.root.join("disc.iso")).unwrap(), *data);
    assert_eq!(hits(&srv, "/signed/a/", heads), 0, "after the restart too, only the new link");
}

#[tokio::test]
async fn the_final_checksum_still_decides_after_a_switch() {
    let data = random_data(4 * MB, 80);
    let srv = serve(data.clone()).await;
    let e = env("sum");
    let m = manager(&e).await;
    let wrong = format!("sha256:{}", "0".repeat(64));
    let failed = expired_part_way(&m, &srv, "a", NewDownload { checksum: Some(wrong), ..signed(&srv, "a") }).await;
    assert!(m.use_new_link(link(failed.id, srv.url("/signed/b/disc.iso"))).await.unwrap().applied);
    let d = wait_status(&m, failed.id, DownloadStatus::Failed).await;
    assert!(d.error.unwrap_or_default().contains("checksum"), "never marked complete");
    assert!(!e.root.join("disc.iso").exists());
}

#[tokio::test]
async fn a_download_refused_before_any_byte_simply_takes_the_new_link() {
    let data = random_data(MB, 81);
    let srv = serve(data.clone()).await;
    let e = env("unstarted");
    let m = manager(&e).await;
    srv.state.expired.lock().unwrap().insert("a".into());
    let d = m.create_download(signed(&srv, "a")).await.unwrap();
    wait_status(&m, d.id, DownloadStatus::Failed).await;
    let o = m.use_new_link(link(d.id, srv.url("/signed/b/disc.iso"))).await.unwrap();
    assert_eq!((o.reason, o.applied, o.kept_bytes), (Reason::NothingToReuse, true, 0));
    wait_status(&m, d.id, DownloadStatus::Completed).await;
    assert_eq!(std::fs::read(e.root.join("disc.iso")).unwrap(), *data);
}
