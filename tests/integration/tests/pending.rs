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

    let d = m
        .confirm_pending(ConfirmPending { token: p.token, filename: None, start: None, folder_token: None, username: None, password: None })
        .await
        .unwrap();
    until("download to finish", || async { m.get_download(d.id).await.unwrap().status == DownloadStatus::Completed }).await;
    assert_eq!(m.get_download(d.id).await.unwrap().filename, "ubuntu.iso");
    assert!(m.list_pending().is_empty(), "the request is gone once confirmed");
    assert!(
        m.confirm_pending(ConfirmPending {
            token: p.token,
            filename: None,
            start: None,
            folder_token: None,
            username: None,
            password: None
        })
        .await
        .is_err(),
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
            m.confirm_pending(ConfirmPending {
                token: p.token,
                filename: Some(bad.into()),
                start: None,
                folder_token: None,
                username: None,
                password: None
            })
            .await
            .is_err(),
            "{bad:?}"
        );
        assert_eq!(m.list_pending().len(), 1, "a refused rename keeps the request waiting");
    }
    let d = m
        .confirm_pending(ConfirmPending {
            token: p.token,
            filename: Some("mine.bin".into()),
            start: Some(false),
            folder_token: None,
            username: None,
            password: None,
        })
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
    let d = m
        .confirm_pending(ConfirmPending { token: p.token, filename: None, start: None, folder_token: None, username: None, password: None })
        .await
        .unwrap();
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

/// A browser download from a site that asks for a user name and password: they are typed in the confirmation,
/// a mistake there leaves the request waiting, and they never show up in what the UI or the logs get.
#[tokio::test]
async fn a_sign_in_typed_in_the_confirmation_is_used_and_never_shown() {
    let data = random_data(MB, 11);
    let srv = serve(data.clone()).await;
    let m = manager("signin").await;
    let p = m.add_pending(req(&srv, "/auth/members.bin"), ctx()).await.unwrap();
    until("the check to finish", || async {
        let l = m.list_pending();
        l.len() == 1 && !l[0].probing && l[0].analysis.is_some()
    })
    .await;
    let found = m.list_pending().remove(0).analysis.unwrap();
    assert_eq!(found.access, download_manager::intelligence::Access::AuthRequired, "the dialog knows the site asks for a sign-in");

    let confirm = |username: Option<&str>, password: Option<&str>| ConfirmPending {
        token: p.token,
        filename: None,
        start: None,
        folder_token: None,
        username: username.map(Into::into),
        password: password.map(Into::into),
    };
    // Mistakes are refused and the request keeps waiting, to be corrected in the same dialog.
    for (u, pw) in [(None, Some("pass")), (Some("us:er"), Some("pass")), (Some("user"), Some("bad\u{7}"))] {
        assert!(m.confirm_pending(confirm(u, pw)).await.is_err(), "{u:?}");
        assert_eq!(m.list_pending().len(), 1, "still waiting after {u:?}");
    }
    let shown = format!("{:?}", confirm(Some("user"), Some("pass")));
    assert!(!shown.contains("pass\"") && !shown.contains("\"user\""), "never logged: {shown}");

    let d = m.confirm_pending(confirm(Some(" user "), Some("pass"))).await.unwrap();
    until("download to finish", || async { m.get_download(d.id).await.unwrap().status == DownloadStatus::Completed }).await;
    let done = m.get_download(d.id).await.unwrap();
    assert!(std::fs::read(&done.destination).unwrap() == data[..], "the protected file, byte for byte");
    let json =
        serde_json::to_string(&done).unwrap() + &serde_json::to_string(&m.list_downloads(ListFilter::default()).await.unwrap()).unwrap();
    assert!(!json.contains("dXNlcjpwYXNz") && !json.contains("\"pass\""), "the UI never gets the sign-in: {json}");
    assert!(m.list_pending().is_empty());
}

#[tokio::test]
async fn without_a_sign_in_the_protected_download_fails_asking_for_one() {
    let srv = serve(random_data(MB, 12)).await;
    let m = manager("nosignin").await;
    let p = m.add_pending(req(&srv, "/auth/members.bin"), ctx()).await.unwrap();
    let d = m
        .confirm_pending(ConfirmPending { token: p.token, filename: None, start: None, folder_token: None, username: None, password: None })
        .await
        .unwrap();
    until("download to fail", || async { m.get_download(d.id).await.unwrap().status == DownloadStatus::Failed }).await;
    assert_eq!(
        m.get_download(d.id).await.unwrap().needs,
        Some(download_manager::explain::Needs::SignIn),
        "the card and details offer Sign in"
    );
}

/// A file only the signed-in browser can get (SharePoint / OneDrive / Office Online, members-only sites) is not
/// taken over from the browser: the check declines it, and the extension lets the browser download it with its
/// own session. Everything VeloxFetch can fetch itself, or cannot tell, is taken as before.
#[tokio::test]
async fn a_file_only_the_signed_in_browser_gets_is_left_to_the_browser() {
    let srv = serve(random_data(MB, 11)).await;
    let m = manager("browser-only").await;
    let named = |path: &str, name: &str| NewDownload { url: srv.url(path), filename: Some(name.into()), ..Default::default() };

    // A sign-in page where the browser was getting a file (the Office Online "Download as CSV" case).
    let page = m.browser_can_take_over(&named("/html/XlFileHandler.aspx", "Template test case(DATA).csv"), &ctx()).await;
    assert!(page.is_err(), "a sign-in page instead of the file is left to the browser");
    // 401 and 403: the server wants the browser's sign-in.
    assert!(m.browser_can_take_over(&req(&srv, "/auth/a.bin"), &ctx()).await.is_err(), "401");
    srv.state.expired.lock().unwrap().insert("tok".into());
    assert!(m.browser_can_take_over(&req(&srv, "/signed/tok/a.bin"), &ctx()).await.is_err(), "403");

    // What VeloxFetch can fetch is taken over.
    assert!(m.browser_can_take_over(&named("/report.csv", "report.csv"), &ctx()).await.is_ok());
    // A web page the user meant to save as a page is not a sign-in page.
    assert!(m.browser_can_take_over(&req(&srv, "/html/page.html"), &BrowserContext::default()).await.is_ok());
    // No answer at all: nothing proves the browser is needed, so the download is taken as before.
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
    let gone = NewDownload { url: format!("http://{closed}/a.bin"), ..Default::default() };
    assert!(m.browser_can_take_over(&gone, &ctx()).await.is_ok());
    // The check never adds anything.
    assert!(m.list_downloads(ListFilter::default()).await.unwrap().is_empty());
}
