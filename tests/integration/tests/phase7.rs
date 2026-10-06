//! Phase 7 (backend half): what a browser hands over (referrer, cookies, user agent) must reach the
//! origin on every request, survive pause / resume / restart, and be wiped when the download ends.
//! The bridge itself is tested in `crates/browser-bridge` and `bridge.rs`.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use download_manager::{BrowserContext, DownloadManager, ListFilter, ManagerConfig, ManagerError, NewDownload};
use downloader_core::{AdaptiveConfig, DownloadStatus, DownloadStore, RetryPolicy};
use persistence::SqliteStore;
use velox_integration_tests::{random_data, serve, Server};

const MB: usize = 1024 * 1024;

struct Env {
    db: PathBuf,
    root: PathBuf,
    /// The credential store the app would use: shared, so a "restarted" manager finds the same key.
    keys: std::sync::Arc<secrets::MemoryKeyStore>,
}

fn env(name: &str) -> Env {
    let base = std::env::temp_dir().join(format!("velox-p7-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    Env { db: base.join("state").join("velox.db"), root: base.join("downloads"), keys: Default::default() }
}

async fn manager(e: &Env) -> DownloadManager {
    let mut c = ManagerConfig::new(e.db.clone(), e.root.clone());
    c.download.min_segment_size = 64 * 1024;
    c.download.checkpoint_interval = Duration::from_millis(50);
    c.download.retry = RetryPolicy { max_retries: 1, max_throttle_retries: 1, ..Default::default() };
    c.download.adaptive = AdaptiveConfig { interval: Duration::from_millis(150), ..Default::default() };
    c.key_store = e.keys.clone();
    DownloadManager::new(c).await.unwrap()
}

fn ctx() -> BrowserContext {
    BrowserContext {
        referrer: Some("https://shop.example/downloads#top".into()),
        cookies: Some("session=s3cr3t; theme=dark".into()),
        user_agent: Some("Mozilla/5.0 TestBrowser/1".into()),
        incognito: false,
    }
}

fn req(srv: &Server, path: &str, bps: Option<u64>) -> NewDownload {
    NewDownload { url: srv.url(path), speed_limit: bps, ..Default::default() }
}

async fn until<F: std::future::Future<Output = bool>>(what: &str, mut f: impl FnMut() -> F) {
    let t = Instant::now();
    while !f().await {
        assert!(t.elapsed() < Duration::from_secs(25), "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_status(m: &DownloadManager, id: i64, want: DownloadStatus) {
    until(&format!("{id} -> {want:?}"), || async { m.get_download(id).await.unwrap().status == want }).await;
}

fn heads(srv: &Server) -> Vec<String> {
    srv.state.request_heads.lock().unwrap().clone()
}

fn stored_headers(e: &Env, id: i64) -> Vec<(String, String)> {
    // Cookies are sealed on disk: reading them back needs the key the manager used.
    let store = SqliteStore::open(&e.db).unwrap();
    store
        .set_sealer(Some(std::sync::Arc::new(secrets::Sealer::from_store(e.keys.as_ref(), download_manager::SECRET_KEY_NAME).unwrap())))
        .unwrap();
    store.load(id).unwrap().unwrap().request_headers
}

#[tokio::test]
async fn browser_headers_reach_every_request_and_the_file_is_intact() {
    let data = random_data(6 * MB, 1);
    let srv = serve(data.clone()).await;
    let e = env("reach");
    let m = manager(&e).await;
    let d = m.create_download_from_browser(req(&srv, "/a.bin", None), ctx()).await.unwrap();
    wait_status(&m, d.id, DownloadStatus::Completed).await;

    let all = heads(&srv);
    assert!(all.len() >= 3, "probe + several ranged requests: {}", all.len());
    for r in &all {
        assert!(r.contains("cookie: session=s3cr3t; theme=dark"), "cookie missing on a request: {r}");
        assert!(r.contains("referer: https://shop.example/downloads\r"), "referer (without fragment) missing: {r}");
        assert!(r.contains("user-agent: mozilla/5.0 testbrowser/1"), "browser user agent missing: {r}");
    }
    assert_eq!(std::fs::read(&m.get_download(d.id).await.unwrap().destination).unwrap(), *data);
}

#[tokio::test]
async fn ordinary_downloads_send_no_browser_headers_regression() {
    let srv = serve(random_data(2 * MB, 2)).await;
    let e = env("plain");
    let m = manager(&e).await;
    let d = m.create_download(req(&srv, "/a.bin", None)).await.unwrap();
    wait_status(&m, d.id, DownloadStatus::Completed).await;
    for r in heads(&srv) {
        assert!(!r.contains("cookie:") && !r.contains("referer:"), "unexpected header: {r}");
        assert!(r.contains("user-agent: velox"), "default user agent: {r}");
    }
    assert!(stored_headers(&e, d.id).is_empty());
}

#[tokio::test]
async fn headers_survive_pause_resume_and_a_restart_then_are_wiped_on_completion() {
    let data = random_data(10 * MB, 3);
    let srv = serve(data.clone()).await;
    let e = env("resume");
    let id;
    {
        let m = manager(&e).await;
        id = m.create_download_from_browser(req(&srv, "/a.bin", Some(2 * MB as u64)), ctx()).await.unwrap().id;
        until("transferring", || async { m.get_download(id).await.unwrap().downloaded > 512 * 1024 }).await;
        m.pause_download(id).await.unwrap();
        assert_eq!(stored_headers(&e, id).len(), 3, "kept while the download is unfinished");
        m.resume_download(id).await.unwrap();
        until("resumed", || async { m.get_download(id).await.unwrap().downloaded > MB as u64 }).await;
        m.shutdown().await;
    }
    let before = heads(&srv).len();
    let m = manager(&e).await;
    m.resume_download(id).await.unwrap();
    wait_status(&m, id, DownloadStatus::Completed).await;
    let after: Vec<String> = heads(&srv).split_off(before);
    assert!(!after.is_empty());
    for r in &after {
        assert!(r.contains("cookie: session=s3cr3t"), "cookie lost after a restart: {r}");
        assert!(r.contains("referer: https://shop.example/downloads"), "referer lost after a restart: {r}");
    }
    assert_eq!(std::fs::read(&m.get_download(id).await.unwrap().destination).unwrap(), *data);
    assert!(stored_headers(&e, id).is_empty(), "cookies must not outlive the download");
}

#[tokio::test]
async fn cancelling_wipes_the_stored_headers() {
    let srv = serve(random_data(10 * MB, 4)).await;
    let e = env("cancel");
    let m = manager(&e).await;
    let id = m.create_download_from_browser(req(&srv, "/a.bin", Some(MB as u64)), ctx()).await.unwrap().id;
    until("transferring", || async { m.get_download(id).await.unwrap().downloaded > 128 * 1024 }).await;
    assert_eq!(stored_headers(&e, id).len(), 3);
    m.cancel_download(id).await.unwrap();
    assert!(stored_headers(&e, id).is_empty());
}

#[tokio::test]
async fn hostile_browser_context_is_rejected_before_anything_is_stored() {
    let srv = serve(random_data(MB, 5)).await;
    let e = env("hostile");
    let m = manager(&e).await;
    let bad = |over: fn(&mut BrowserContext)| {
        let mut c = ctx();
        over(&mut c);
        c
    };
    for c in [
        bad(|c| c.cookies = Some("a=b\r\nX-Evil: 1".into())),
        bad(|c| c.user_agent = Some("ua\nX-Evil: 1".into())),
        bad(|c| c.referrer = Some("javascript:alert(1)".into())),
        bad(|c| c.referrer = Some("file:///C:/Windows/win.ini".into())),
        bad(|c| c.incognito = true), // cookies from a private window
    ] {
        let r = m.create_download_from_browser(req(&srv, "/a.bin", None), c).await;
        assert!(matches!(r, Err(ManagerError::InvalidInput(_))), "{r:?}");
    }
    // The URL is validated exactly as for the UI.
    let r = m.create_download_from_browser(NewDownload { url: "file:///C:/Windows/win.ini".into(), ..Default::default() }, ctx()).await;
    assert!(matches!(r, Err(ManagerError::InvalidInput(_))));
    assert!(m.list_downloads(ListFilter::default()).await.unwrap().is_empty(), "nothing was stored");
    assert!(srv.state.request_heads.lock().unwrap().is_empty(), "and nothing touched the network");
}
