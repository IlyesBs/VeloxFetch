//! Download Intelligence against a real (local) server: what the check before a download finds, that it
//! downloads and writes nothing, that answering or a new check stops it, and that the download itself still
//! works afterwards.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use download_manager::intelligence::{
    Access, AnalysisDto, AnalyzeLink, DuplicateReason, FindingCode, Health, ResourceKind, ResumeSupport, StorageCheck, Strategy,
};
use download_manager::{BrowserContext, ConfirmPending, DownloadManager, ListFilter, ManagerConfig, ManagerEvent, NewDownload};
use downloader_core::DownloadStatus;
use velox_integration_tests::{random_data, serve};

const MB: usize = 1024 * 1024;

fn paths(name: &str) -> (PathBuf, PathBuf) {
    let base = std::env::temp_dir().join(format!("velox-intel-{}-{name}", std::process::id()));
    (base.join("state").join("velox.db"), base.join("downloads"))
}

async fn manager(name: &str) -> DownloadManager {
    let (db, root) = paths(name);
    let _ = std::fs::remove_dir_all(db.parent().unwrap().parent().unwrap());
    let mut c = ManagerConfig::new(db, root);
    c.download.min_segment_size = 64 * 1024;
    DownloadManager::new(c).await.unwrap()
}

async fn check(m: &DownloadManager, url: String) -> AnalysisDto {
    m.analyze_link(AnalyzeLink { url: Some(url) }).await.unwrap().unwrap()
}

fn codes(a: &AnalysisDto) -> Vec<FindingCode> {
    a.findings.iter().map(|f| f.code).collect()
}

/// Nothing was created in the download folder.
fn nothing_written(name: &str) -> bool {
    std::fs::read_dir(paths(name).1).map_or(true, |d| d.count() == 0)
}

async fn until<F: std::future::Future<Output = bool>>(what: &str, mut f: impl FnMut() -> F) {
    let t = Instant::now();
    while !f().await {
        assert!(t.elapsed() < Duration::from_secs(20), "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn a_normal_file_with_ranges_and_an_etag() {
    let srv = serve(random_data(8 * MB, 1)).await;
    let m = manager("normal").await;
    let a = check(&m, srv.url("/pkg/app.zip")).await;
    assert_eq!(a.kind, ResourceKind::File);
    assert_eq!(a.filename, "app.zip");
    assert_eq!(a.size, Some((8 * MB) as u64));
    assert!(a.range_verified && a.range_claimed && a.etag, "{a:?}");
    assert_eq!(a.resume, ResumeSupport::Supported);
    assert_eq!(a.strategy, Strategy::Parallel);
    assert_eq!(a.connections, Some(4), "the engine's own start for 8 MB");
    assert_eq!(a.access, Access::Public);
    assert_eq!(a.storage, StorageCheck::Ok);
    assert!(a.free_bytes.is_some());
    assert!(!a.https && codes(&a) == [FindingCode::Insecure], "plain http is the only remark: {:?}", a.findings);
    assert_eq!(a.health, Health::Caution);
    assert_eq!(srv.state.hits.load(Ordering::SeqCst), 0, "the file itself was never requested");
    assert!(nothing_written("normal"));
    assert!(m.list_downloads(ListFilter::default()).await.unwrap().is_empty());
}

#[tokio::test]
async fn no_ranges_no_resume_one_connection() {
    let srv = serve(random_data(2 * MB, 2)).await;
    let m = manager("norange").await;
    let a = check(&m, srv.url("/norange/a.bin")).await;
    assert_eq!(a.resume, ResumeSupport::NotSupported);
    assert_eq!(a.strategy, Strategy::Single);
    assert_eq!(a.connections, None);
    assert!(codes(&a).contains(&FindingCode::NoResume));
}

#[tokio::test]
async fn redirects_are_followed_and_reported_by_host_only() {
    let srv = serve(random_data(MB, 3)).await;
    let m = manager("redir").await;
    let a = check(&m, srv.url("/redir/2/f.bin")).await;
    assert!(a.redirected);
    assert_eq!(a.final_host.as_deref(), Some("127.0.0.1"));
    assert_eq!(a.resume, ResumeSupport::Supported);
    assert!(!codes(&a).contains(&FindingCode::OtherSite), "same site");
}

#[tokio::test]
async fn sign_in_web_pages_and_server_errors() {
    let srv = serve(random_data(MB, 4)).await;
    let m = manager("auth").await;
    let a = check(&m, srv.url("/auth/secret.bin")).await;
    assert_eq!((a.access, a.health), (Access::AuthRequired, Health::Problem));
    assert_eq!(a.error_code.as_deref(), Some("http_401"));
    // With the user name and password in the link, the same check passes.
    let ok = check(&m, srv.url("/auth/secret.bin").replace("http://", "http://user:pass@")).await;
    assert_eq!(ok.resume, ResumeSupport::Supported);
    assert_ne!(ok.access, Access::AuthRequired);
    let page = check(&m, srv.url("/html/report.pdf")).await;
    assert_eq!((page.kind, page.health), (ResourceKind::WebPage, Health::Problem));
    let down = check(&m, srv.url("/e500/x.bin")).await;
    assert_eq!(down.error_code.as_deref(), Some("http_500"));
    assert_eq!((down.kind, down.resume, down.strategy), (ResourceKind::Unknown, ResumeSupport::Unknown, Strategy::Conservative));
    assert!(nothing_written("auth"));
}

#[tokio::test]
async fn a_size_that_cannot_fit_is_flagged() {
    let srv = serve(random_data(MB, 5)).await;
    let m = manager("huge").await;
    let a = check(&m, srv.url("/huge/big.iso")).await;
    assert_eq!(a.size, Some(1 << 50));
    assert_eq!((a.storage, a.health), (StorageCheck::Insufficient, Health::Problem));
    let f = a.findings.iter().find(|f| f.code == FindingCode::NoSpace).unwrap();
    assert_eq!((f.bytes, f.free), (Some(1 << 50), a.free_bytes));
    assert!(nothing_written("huge"), "nothing reserved");
}

#[tokio::test]
async fn existing_downloads_are_found_by_link_not_by_name() {
    let srv = serve(random_data(MB, 6)).await;
    let m = manager("dup").await;
    let d = m.create_download(NewDownload { url: srv.url("/a/setup.exe"), start: Some(false), ..Default::default() }).await.unwrap();
    let a = check(&m, srv.url("/a/setup.exe#again")).await;
    let dup = a.duplicate.clone().expect("same link");
    assert_eq!((dup.id, dup.reason), (Some(d.id), DuplicateReason::SameLink));
    assert!(codes(&a).contains(&FindingCode::Duplicate));
    let other = check(&m, srv.url("/b/setup.exe")).await;
    assert_eq!(other.duplicate, None, "same file name, different link: not a duplicate");
    // A redirect to a link already in the list.
    let r = check(&m, srv.url("/redir/1/a/setup.exe")).await;
    assert_eq!(r.duplicate.map(|d| d.reason), Some(DuplicateReason::SameFinalLink));
}

#[tokio::test]
async fn a_temporary_link_is_detected_and_its_signature_never_shown() {
    let srv = serve(random_data(MB, 7)).await;
    let m = manager("temp").await;
    let url = srv.url("/f.bin?X-Amz-Date=20991231T000000Z&X-Amz-Expires=600&X-Amz-Signature=deadbeef");
    let a = check(&m, url.clone()).await;
    assert!(a.temporary && a.expires_at_ms.is_some());
    assert!(codes(&a).contains(&FindingCode::TemporaryLink));
    let json = serde_json::to_string(&a).unwrap();
    assert!(!json.contains("deadbeef"), "the analysis carries no signature: {json}");
    // The confirmation dialog shows the link without it.
    let p = m.add_pending(NewDownload { url, ..Default::default() }, BrowserContext::default()).await.unwrap();
    assert!(!p.url.contains("deadbeef") && p.url.contains("/f.bin"), "{}", p.url);
}

#[tokio::test]
async fn the_confirmation_dialog_gets_the_analysis_and_the_download_still_works() {
    let srv = serve(random_data(2 * MB, 8)).await;
    let m = manager("pending").await;
    let ctx = BrowserContext { referrer: None, cookies: Some("sid=1".into()), user_agent: None, incognito: false };
    let p = m.add_pending(NewDownload { url: srv.url("/movie.mkv"), ..Default::default() }, ctx).await.unwrap();
    assert!(p.analysis.is_none() && p.probing);
    until("the analysis", || async { m.list_pending().first().is_some_and(|p| p.analysis.is_some()) }).await;
    let p = m.list_pending().remove(0);
    let a = p.analysis.clone().unwrap();
    assert_eq!(a.resume, ResumeSupport::Supported);
    assert_eq!(p.resumable, Some(true), "the older field still says the same");
    assert_eq!(a.access, Access::Unknown, "a browser session was sent: whether it is needed is not known");
    assert!(codes(&a).contains(&FindingCode::SessionSent));
    assert!(!serde_json::to_string(&a).unwrap().contains("sid=1"), "the cookie never reaches the result");
    assert!(nothing_written("pending"));
    let d = m
        .confirm_pending(ConfirmPending {
            token: p.token,
            filename: None,
            start: None,
            remember_for: None,
            folder_token: None,
            username: None,
            password: None,
        })
        .await
        .unwrap();
    until("the download", || async { m.get_download(d.id).await.unwrap().status == DownloadStatus::Completed }).await;
}

#[tokio::test]
async fn a_failed_analysis_leaves_the_download_available() {
    let srv = serve(random_data(MB, 9)).await;
    let m = manager("failed").await;
    let p = m.add_pending(NewDownload { url: srv.url("/e500/x.bin"), ..Default::default() }, BrowserContext::default()).await.unwrap();
    until("the analysis", || async { m.list_pending().first().is_some_and(|p| p.analysis.is_some()) }).await;
    let a = m.list_pending()[0].analysis.clone().unwrap();
    assert_eq!(a.error_code.as_deref(), Some("http_500"));
    // The user can still add it (the download then reports the server's error the usual way).
    let d = m
        .confirm_pending(ConfirmPending {
            token: p.token,
            filename: None,
            start: Some(false),
            remember_for: None,
            folder_token: None,
            username: None,
            password: None,
        })
        .await
        .unwrap();
    assert_eq!(m.get_download(d.id).await.unwrap().status, DownloadStatus::Queued);
}

#[tokio::test]
async fn answering_or_a_new_check_cancels_the_analysis() {
    let srv = serve(random_data(MB, 10)).await;
    *srv.state.head_delay.lock().unwrap() = Duration::from_secs(5);
    let m = manager("cancel").await;
    let mut events = m.subscribe();
    // The dialog is answered while the server is still silent: the analysis stops, nothing comes back.
    let p = m.add_pending(NewDownload { url: srv.url("/slow.bin"), ..Default::default() }, BrowserContext::default()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    m.dismiss_pending(p.token).unwrap();
    // The Add dialog: a new check (or none) cancels the one running.
    let m2 = m.clone();
    let first = tokio::spawn(async move { m2.analyze_link(AnalyzeLink { url: Some(srv.url("/slow2.bin")) }).await });
    tokio::time::sleep(Duration::from_millis(200)).await;
    let t = Instant::now();
    assert_eq!(m.analyze_link(AnalyzeLink { url: None }).await.unwrap(), None);
    let res = first.await.unwrap();
    assert!(res.is_err(), "the first check was cancelled: {res:?}");
    assert!(t.elapsed() < Duration::from_secs(2), "cancelled at once, not after the server answered");
    tokio::time::sleep(Duration::from_millis(300)).await;
    while let Ok(ev) = events.try_recv() {
        if let ManagerEvent::Pending { pending } = ev {
            assert!(pending.analysis.is_none(), "no result for a dismissed request");
        }
    }
    assert!(m.list_pending().is_empty());
    assert!(m.analyze_link(AnalyzeLink { url: Some("file:///etc/passwd".into()) }).await.is_err(), "only http(s) links");
}
