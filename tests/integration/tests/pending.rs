//! Browser downloads wait for the user's confirmation: nothing is downloaded or stored until the user
//! says yes, the dialog gets the file's details, and hostile input is refused like for any download.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use download_manager::{BrowserContext, ConfirmPending, DownloadManager, ListFilter, ManagerConfig, ManagerEvent, NewDownload};
use downloader_core::DownloadStatus;
use velox_integration_tests::{random_data, serve, Server};

const MB: usize = 1024 * 1024;

fn paths(name: &str) -> (PathBuf, PathBuf) {
    let base = std::env::temp_dir().join(format!("velox-pending-{}-{name}", std::process::id()));
    (base.join("state").join("velox.db"), base.join("downloads"))
}

async fn manager(name: &str) -> DownloadManager {
    let (db, root) = paths(name);
    let _ = std::fs::remove_dir_all(db.parent().unwrap().parent().unwrap());
    let mut c = ManagerConfig::new(db, root);
    c.download.min_segment_size = 64 * 1024;
    DownloadManager::new(c).await.unwrap()
}

fn ctx() -> BrowserContext {
    BrowserContext { referrer: Some("https://shop.example/p".into()), cookies: Some("sid=1".into()), user_agent: None, incognito: false }
}

fn req(srv: &Server, path: &str) -> NewDownload {
    NewDownload { url: srv.url(path), ..Default::default() }
}

async fn until<F: std::future::Future<Output = bool>>(what: &str, mut f: impl FnMut() -> F) {
    let t = Instant::now();
    while !f().await {
        assert!(t.elapsed() < Duration::from_secs(20), "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn nothing_starts_until_the_user_confirms() {
    let srv = serve(random_data(2 * MB, 7)).await;
    let m = manager("confirm").await;
    let mut events = m.subscribe();

    let p = m.add_pending(req(&srv, "/ubuntu.iso"), ctx()).await.unwrap();
    assert_eq!(p.filename, "ubuntu.iso");
    assert!(p.probing, "the details are still being fetched");
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(m.list_downloads(ListFilter::default()).await.unwrap().is_empty(), "no download exists yet");
    assert!(std::fs::read_dir(m_root(&m, "confirm")).map_or(true, |d| d.count() == 0), "nothing on disk yet");

    // The dialog gets size, type and resume support from the server.
    until("details to arrive", || async {
        let l = m.list_pending();
        l.len() == 1 && !l[0].probing && l[0].size == Some((2 * MB) as u64)
    })
    .await;
    let details = m.list_pending().remove(0);
    assert_eq!(details.resumable, Some(true));
    assert!(details.content_type.is_some());
    assert_eq!(details.host, "127.0.0.1");

    // Events for the UI: added, then updated with the details.
    let mut saw_pending = 0;
    while let Ok(ev) = events.try_recv() {
        if matches!(ev, ManagerEvent::Pending { .. }) {
            saw_pending += 1;
        }
    }
    assert!(saw_pending >= 2, "the UI is told twice: {saw_pending}");

    let d = m.confirm_pending(ConfirmPending { token: p.token, filename: None, start: None, folder_token: None }).await.unwrap();
    until("download to finish", || async { m.get_download(d.id).await.unwrap().status == DownloadStatus::Completed }).await;
    assert_eq!(m.get_download(d.id).await.unwrap().filename, "ubuntu.iso");
    assert!(m.list_pending().is_empty(), "the request is gone once confirmed");
    assert!(
        m.confirm_pending(ConfirmPending { token: p.token, filename: None, start: None, folder_token: None }).await.is_err(),
        "cannot confirm twice"
    );
}

fn m_root(_m: &DownloadManager, name: &str) -> PathBuf {
    paths(name).1
}

#[tokio::test]
async fn cancel_downloads_nothing() {
    let srv = serve(random_data(MB, 8)).await;
    let m = manager("cancel").await;
    let p = m.add_pending(req(&srv, "/x.bin"), ctx()).await.unwrap();
    m.dismiss_pending(p.token).unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(m.list_pending().is_empty());
    assert!(m.list_downloads(ListFilter::default()).await.unwrap().is_empty());
    assert!(m.dismiss_pending(p.token).is_err(), "already answered");
    // The background probe of a dismissed request must not resurrect it.
    assert!(m.list_pending().is_empty());
}

#[tokio::test]
async fn the_user_can_rename_and_the_name_is_validated() {
    let srv = serve(random_data(MB, 9)).await;
    let m = manager("rename").await;
    let p = m.add_pending(req(&srv, "/a.bin"), ctx()).await.unwrap();
    for bad in ["..\\..\\evil.exe", "a/b.bin", "CON", "x\0y"] {
        assert!(
            m.confirm_pending(ConfirmPending { token: p.token, filename: Some(bad.into()), start: None, folder_token: None })
                .await
                .is_err(),
            "{bad:?}"
        );
        assert_eq!(m.list_pending().len(), 1, "a refused rename keeps the request waiting");
    }
    let d = m
        .confirm_pending(ConfirmPending { token: p.token, filename: Some("mine.bin".into()), start: Some(false), folder_token: None })
        .await
        .unwrap();
    assert_eq!(d.filename, "mine.bin");
    assert_ne!(d.status, DownloadStatus::Downloading, "start: false only adds it to the list");
}

#[tokio::test]
async fn hostile_input_is_refused_and_the_list_is_bounded() {
    let srv = serve(random_data(1024, 10)).await;
    let m = manager("hostile").await;
    for url in ["javascript:alert(1)", "file:///C:/Windows/win.ini", "ftp://x.test/a", "not a url"] {
        let r = m.add_pending(NewDownload { url: url.into(), ..Default::default() }, ctx()).await;
        assert!(r.is_err(), "{url}");
    }
    let bad_ctx = BrowserContext { cookies: Some("a=b\r\nX: 1".into()), ..ctx() };
    assert!(m.add_pending(req(&srv, "/a"), bad_ctx).await.is_err());
    let private = BrowserContext { incognito: true, ..ctx() };
    assert!(m.add_pending(req(&srv, "/a"), private).await.is_err(), "cookies from a private window are refused");
    assert!(m.list_pending().is_empty());

    for i in 0..50 {
        m.add_pending(req(&srv, &format!("/f{i}.bin")), BrowserContext::default()).await.unwrap();
    }
    assert!(m.add_pending(req(&srv, "/one-too-many.bin"), BrowserContext::default()).await.is_err(), "at most 50 wait at once");
}

#[tokio::test]
async fn the_confirmed_download_carries_the_browser_headers() {
    let srv = serve(random_data(MB, 11)).await;
    let m = manager("headers").await;
    let p = m.add_pending(req(&srv, "/h.bin"), ctx()).await.unwrap();
    let d = m.confirm_pending(ConfirmPending { token: p.token, filename: None, start: None, folder_token: None }).await.unwrap();
    until("finish", || async { m.get_download(d.id).await.unwrap().status == DownloadStatus::Completed }).await;
    let heads = srv.state.request_heads.lock().unwrap().clone();
    assert!(heads.iter().all(|h| h.contains("cookie: sid=1") && h.contains("referer: https://shop.example/p")), "{heads:?}");
}

#[tokio::test]
async fn the_confirmation_setting_defaults_to_on_and_persists() {
    let m = manager("setting").await;
    assert!(m.browser_confirm(), "asking is the default");
    m.set_browser_confirm(false).await.unwrap();
    assert!(!m.browser_confirm());
    m.shutdown().await;
    let (db, root) = paths("setting");
    let again = DownloadManager::new(ManagerConfig::new(db, root)).await.unwrap();
    assert!(!again.browser_confirm(), "kept across restarts");
    again.set_browser_confirm(true).await.unwrap();
    assert!(again.browser_confirm());
}
