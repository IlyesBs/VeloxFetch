//! Phase 11: secrets and credentials. Cookies and passwords never reach the disk in the clear,
//! never appear in links, DTOs or debug output, and are dropped rather than stored unprotected.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use download_manager::{BrowserContext, DownloadManager, ManagerConfig, NewDownload};
use downloader_core::{DownloadStatus, RetryPolicy};
use secrets::{KeyStore, MemoryKeyStore, UnavailableKeyStore};
use velox_integration_tests::{random_data, serve, Server};

struct Env {
    root: PathBuf,
    db: PathBuf,
}

fn env(name: &str) -> Env {
    let base = std::env::temp_dir().join(format!("velox-p11-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    Env { root: base.join("downloads"), db: base.join("state").join("velox.db") }
}

async fn manager(e: &Env, keys: Arc<dyn KeyStore>) -> DownloadManager {
    let mut c = ManagerConfig::new(&e.db, &e.root);
    c.progress_interval = Duration::from_millis(40);
    c.download.checkpoint_interval = Duration::from_millis(50);
    c.download.retry = RetryPolicy {
        max_retries: 1,
        max_throttle_retries: 1,
        base_delay: Duration::from_millis(5),
        max_delay: Duration::from_millis(20),
        max_retry_after: Duration::from_millis(50),
    };
    c.key_store = keys;
    DownloadManager::new(c).await.unwrap()
}

async fn wait_final(m: &DownloadManager, id: i64) -> download_manager::DownloadDto {
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

/// Every byte the database left on disk.
fn disk_bytes(db: &std::path::Path) -> Vec<u8> {
    let mut all = Vec::new();
    for ext in ["", "-wal", "-shm"] {
        let mut p = db.as_os_str().to_owned();
        p.push(ext);
        all.extend(std::fs::read(p).unwrap_or_default());
    }
    all
}

fn contains(hay: &[u8], needle: &str) -> bool {
    hay.windows(needle.len()).any(|w| w == needle.as_bytes())
}

fn heads(srv: &Server) -> String {
    srv.state.request_heads.lock().unwrap().join("\n")
}

fn browser_dl(srv: &Server, path: &str) -> (NewDownload, BrowserContext) {
    (
        NewDownload { url: srv.url(path), start: Some(false), ..Default::default() },
        BrowserContext {
            referrer: Some("https://site.test/p".into()),
            cookies: Some("sid=COOKIE-SECRET-123".into()),
            user_agent: None,
            incognito: false,
        },
    )
}

#[tokio::test]
async fn cookies_survive_a_restart_but_only_sealed() {
    let e = env("restart");
    let srv = serve(random_data(50_000, 1)).await;
    let keys: Arc<dyn KeyStore> = Arc::new(MemoryKeyStore::default());
    let m = manager(&e, keys.clone()).await;
    assert!(m.secrets_protected());
    let (new, ctx) = browser_dl(&srv, "/a.bin");
    let queued = m.create_download_from_browser(new, ctx).await.unwrap();
    m.shutdown().await;
    drop(m);
    let bytes = disk_bytes(&e.db);
    for secret in ["COOKIE-SECRET-123", "sid=", "site.test/p"] {
        assert!(!contains(&bytes, secret), "{secret} is in the database file in the clear");
    }

    // Same key store (as after a real restart): the cookie is back and is sent.
    let m = manager(&e, keys.clone()).await;
    m.start_download(queued.id).await.unwrap();
    assert_eq!(wait_final(&m, queued.id).await.status, DownloadStatus::Completed);
    assert!(heads(&srv).contains("cookie: sid=cookie-secret-123"), "the resumed download authenticated");
    m.shutdown().await;
}

#[tokio::test]
async fn a_lost_key_means_no_cookie_not_a_leak_or_a_crash() {
    let e = env("lostkey");
    let srv = serve(random_data(20_000, 2)).await;
    let m = manager(&e, Arc::new(MemoryKeyStore::default())).await;
    let (new, ctx) = browser_dl(&srv, "/b.bin");
    let queued = m.create_download_from_browser(new, ctx).await.unwrap();
    m.shutdown().await;
    drop(m);

    // A different key (credential store wiped, profile restored on another PC).
    let m = manager(&e, Arc::new(MemoryKeyStore::default())).await;
    m.start_download(queued.id).await.unwrap();
    assert_eq!(wait_final(&m, queued.id).await.status, DownloadStatus::Completed, "the download itself still works");
    assert!(!heads(&srv).contains("cookie-secret"), "an unreadable cookie is dropped, not sent as garbage");
    m.shutdown().await;
}

#[tokio::test]
async fn without_a_credential_store_secrets_stay_in_memory_and_never_touch_the_disk() {
    let e = env("nostore");
    let srv = serve(random_data(20_000, 3)).await;
    let m = manager(&e, Arc::new(UnavailableKeyStore)).await;
    assert!(!m.secrets_protected(), "the UI is told");
    let (mut new, ctx) = browser_dl(&srv, "/c.bin");
    new.start = Some(true); // used right away, in memory
    let d = m.create_download_from_browser(new, ctx).await.unwrap();
    assert_eq!(wait_final(&m, d.id).await.status, DownloadStatus::Completed);
    assert!(heads(&srv).contains("cookie: sid=cookie-secret-123"), "still works within the session");
    m.shutdown().await;
    drop(m);
    let bytes = disk_bytes(&e.db);
    assert!(!contains(&bytes, "COOKIE-SECRET-123") && !contains(&bytes, "sid="), "nothing in the clear, and nothing sealed either");
}

#[tokio::test]
async fn a_password_in_the_link_becomes_a_sealed_authorization_header() {
    let e = env("urlcreds");
    let srv = serve(random_data(30_000, 4)).await;
    let keys: Arc<dyn KeyStore> = Arc::new(MemoryKeyStore::default());
    let m = manager(&e, keys.clone()).await;
    let link = format!("http://alice:p%40ss-W0RD-xyz@{}/d.bin", srv.addr);
    let queued = m.create_download(NewDownload { url: link.clone(), start: Some(false), ..Default::default() }).await.unwrap();

    // Not in the stored / shown link, not in what the UI receives, not in the debug output, not on disk.
    assert!(!queued.url.contains("alice") && !queued.url.contains("W0RD"), "{}", queued.url);
    let json = serde_json::to_string(&m.list_downloads(Default::default()).await.unwrap()).unwrap();
    assert!(!json.contains("W0RD") && !json.contains("alice") && !json.contains("authorization"), "{json}");
    let debug = format!("{:?}", NewDownload { url: link, ..Default::default() });
    assert!(!debug.contains("W0RD") && !debug.contains("alice"), "{debug}");
    m.shutdown().await;
    drop(m);
    let bytes = disk_bytes(&e.db);
    for secret in ["W0RD", "alice", "YWxpY2U6", "Basic"] {
        assert!(!contains(&bytes, secret), "{secret} reached the database in the clear");
    }

    // And it still authenticates, after a restart.
    let m = manager(&e, keys).await;
    m.start_download(queued.id).await.unwrap();
    assert_eq!(wait_final(&m, queued.id).await.status, DownloadStatus::Completed);
    let expected = format!("authorization: {}", http_client::basic_authorization("alice", "p@ss-W0RD-xyz").to_lowercase());
    assert!(heads(&srv).contains(&expected), "the header was sent: {}", heads(&srv));
    m.shutdown().await;
}

#[tokio::test]
async fn finished_downloads_keep_no_secrets_and_streams_refuse_passwords() {
    let e = env("finished");
    let srv = serve(random_data(10_000, 5)).await;
    let m = manager(&e, Arc::new(MemoryKeyStore::default())).await;
    let (mut new, ctx) = browser_dl(&srv, "/f.bin");
    new.start = Some(true);
    let d = m.create_download_from_browser(new, ctx).await.unwrap();
    assert_eq!(wait_final(&m, d.id).await.status, DownloadStatus::Completed);
    m.shutdown().await;
    drop(m);
    let bytes = disk_bytes(&e.db);
    assert!(!contains(&bytes, "COOKIE-SECRET") && !contains(&bytes, "sid="), "no cookie in any form once the download is over");

    let m = manager(&e, Arc::new(MemoryKeyStore::default())).await;
    let r = m.analyze_media(format!("http://u:p@{}/x.m3u8", srv.addr)).await;
    assert!(r.is_err(), "stream links with a password are refused");
    m.shutdown().await;
}
