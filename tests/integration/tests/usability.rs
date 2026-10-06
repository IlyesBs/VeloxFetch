//! Features that make the app usable day to day, end to end against the test server:
//!
//! * the Add dialog's advanced options (sign-in, referrer, cookies, user agent) reach the server, sealed on disk;
//! * a server that asks for a password: the download says so plainly and "sign in and retry" finishes it;
//! * network settings: proxy (none / system / manual, with a sealed password), user agent; checked strictly;
//!   survive a restart; a download started afterwards really goes through the proxy;
//! * speed limits per kind of file;
//! * show in folder / open (dry run): paths from the database only, programs never opened;
//! * history lines can be removed without losing the lifetime total;
//! * errors in plain words.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use download_manager::{
    explain::Needs, DownloadDto, DownloadManager, FileTarget, ManagerConfig, NewDownload, ProxyMode, SetCredentials, SetNetwork,
    SetSpeedLimit, SpeedTarget, SystemHandle,
};
use downloader_core::{DownloadStatus, FileKind};
use system_integration::DryRunSystem;
use velox_integration_tests::{random_data, serve, Server};

struct Env {
    base: PathBuf,
    m: DownloadManager,
    srv: Server,
    data: Arc<Vec<u8>>,
    shell: Arc<DryRunSystem>,
    keys: Arc<secrets::MemoryKeyStore>,
}

fn base_dir(name: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!("velox-usability-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    base
}

fn config(base: &Path, shell: &Arc<DryRunSystem>, keys: &Arc<secrets::MemoryKeyStore>) -> ManagerConfig {
    let mut c = ManagerConfig::new(base.join("state").join("velox.db"), base.join("downloads"));
    c.progress_interval = Duration::from_millis(40);
    c.download.min_segment_size = 64 * 1024;
    c.system = SystemHandle(shell.clone());
    c.key_store = keys.clone();
    c
}

async fn env(name: &str) -> Env {
    let base = base_dir(name);
    let data = random_data(200 * 1024, 77);
    let srv = serve(data.clone()).await;
    let shell = Arc::new(DryRunSystem::default());
    let keys = Arc::new(secrets::MemoryKeyStore::default());
    let m = DownloadManager::new(config(&base, &shell, &keys)).await.unwrap();
    Env { base, m, srv, data, shell, keys }
}

async fn restart(e: &mut Env) {
    e.m.shutdown().await;
    e.m = DownloadManager::new(config(&e.base, &e.shell, &e.keys)).await.unwrap();
}

async fn settle(m: &DownloadManager, id: i64) -> DownloadDto {
    let t = Instant::now();
    loop {
        let d = m.get_download(id).await.unwrap();
        if matches!(d.status, DownloadStatus::Completed | DownloadStatus::Failed) {
            return d;
        }
        assert!(t.elapsed() < Duration::from_secs(30), "stuck in {:?}", d.status);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn heads(srv: &Server) -> Vec<String> {
    srv.state.request_heads.lock().unwrap().clone()
}

// ============================================================================= Add dialog: advanced options

#[tokio::test]
async fn sign_in_referrer_cookies_and_user_agent_typed_in_the_add_dialog_reach_the_server() {
    let e = env("advanced").await;
    let d =
        e.m.create_download(NewDownload {
            url: e.srv.url("/auth/file.bin"),
            username: Some("user".into()),
            password: Some("pass".into()),
            referrer: Some("https://site.test/page".into()),
            cookies: Some("sid=42".into()),
            user_agent: Some("Mozilla/5.0 Test".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let d = settle(&e.m, d.id).await;
    assert_eq!(d.status, DownloadStatus::Completed, "{:?}", d.error);
    assert_eq!(std::fs::read(&d.destination).unwrap(), *e.data);
    let h = heads(&e.srv);
    assert!(h.iter().all(|r| r.contains("authorization: basic dxnlcjpwyxnz")), "every request signs in");
    assert!(h.iter().all(|r| r.contains("referer: https://site.test/page")));
    assert!(h.iter().all(|r| r.contains("cookie: sid=42")));
    assert!(h.iter().all(|r| r.contains("user-agent: mozilla/5.0 test")));

    // The password never reaches the database in the clear.
    e.m.shutdown().await;
    let db = std::fs::read(e.base.join("state").join("velox.db")).unwrap();
    let wal = std::fs::read(e.base.join("state").join("velox.db-wal")).unwrap_or_default();
    for blob in [&db, &wal] {
        let text = String::from_utf8_lossy(blob);
        assert!(!text.contains("dXNlcjpwYXNz") && !text.contains("sid=42"), "credentials are sealed on disk");
    }
}

#[tokio::test]
async fn bad_advanced_options_are_refused_and_nothing_is_created() {
    let e = env("advanced-bad").await;
    for bad in [
        NewDownload { url: e.srv.url("/f"), password: Some("pw".into()), ..Default::default() },
        NewDownload { url: e.srv.url("/f"), username: Some("a:b".into()), ..Default::default() },
        NewDownload { url: e.srv.url("/f"), cookies: Some("a=1\r\nX-Evil: 1".into()), ..Default::default() },
        NewDownload { url: e.srv.url("/f"), user_agent: Some("bad\nagent".into()), ..Default::default() },
        NewDownload { url: e.srv.url("/f"), referrer: Some("javascript:alert(1)".into()), ..Default::default() },
    ] {
        assert!(e.m.create_download(bad).await.is_err());
    }
    assert!(e.m.list_downloads(Default::default()).await.unwrap().is_empty());
}

// ============================================================================= a server that asks for a password

#[tokio::test]
async fn a_server_asking_for_a_password_says_so_and_sign_in_and_retry_finishes_the_file() {
    let e = env("signin").await;
    let d = e.m.create_download(NewDownload { url: e.srv.url("/auth/secret.bin"), ..Default::default() }).await.unwrap();
    let d = settle(&e.m, d.id).await;
    assert_eq!(d.status, DownloadStatus::Failed);
    assert_eq!(d.needs, Some(Needs::SignIn));
    assert_eq!(d.error.as_deref(), Some("The server asks for a user name and password."));
    assert!(d.error_detail.as_deref().unwrap().contains("401"), "the technical message stays available");

    // Wrong password: still asks.
    e.m.set_download_credentials(SetCredentials { id: d.id, username: "user".into(), password: "nope".into() }).await.unwrap();
    let again = settle(&e.m, d.id).await;
    assert_eq!((again.status, again.needs), (DownloadStatus::Failed, Some(Needs::SignIn)));

    // Right one: done, byte for byte.
    e.m.set_download_credentials(SetCredentials { id: d.id, username: "user".into(), password: "pass".into() }).await.unwrap();
    let ok = settle(&e.m, d.id).await;
    assert_eq!(ok.status, DownloadStatus::Completed, "{:?}", ok.error);
    assert_eq!((ok.error, ok.needs), (None, None));
    assert_eq!(std::fs::read(&ok.destination).unwrap(), *e.data);

    // Bad input and wrong states are refused.
    assert!(e.m.set_download_credentials(SetCredentials { id: d.id, username: "user".into(), password: "pass".into() }).await.is_err());
    assert!(e.m.set_download_credentials(SetCredentials { id: 999, username: "u".into(), password: "p".into() }).await.is_err());
}

#[tokio::test]
async fn common_failures_are_explained_in_plain_words() {
    let e = env("explain").await;
    let d = e.m.create_download(NewDownload { url: e.srv.url("/404/gone.zip"), ..Default::default() }).await.unwrap();
    let d = settle(&e.m, d.id).await;
    assert_eq!(d.status, DownloadStatus::Failed);
    assert!(d.error.as_deref().unwrap().contains("not on the server"), "{:?}", d.error);
    assert_eq!(d.needs, None);
    assert_eq!(d.kind, FileKind::Archive, "the kind comes from the name even before anything was downloaded");
}

// ============================================================================= network settings

fn net(mode: ProxyMode, url: Option<&str>, user: Option<&str>, pass: Option<&str>, ua: Option<&str>) -> SetNetwork {
    SetNetwork {
        proxy_mode: mode,
        proxy_url: url.map(Into::into),
        proxy_username: user.map(Into::into),
        proxy_password: pass.map(Into::into),
        user_agent: ua.map(Into::into),
    }
}

#[tokio::test]
async fn a_manual_proxy_with_a_password_carries_new_downloads_and_survives_a_restart() {
    let mut e = env("proxy").await;
    let before = e.m.get_network();
    assert_eq!((before.proxy_mode, before.proxy_url.as_deref(), before.user_agent.as_deref()), (ProxyMode::System, None, None));
    assert!(before.default_user_agent.starts_with("VeloxFetch/"));

    // The test server plays the proxy: an HTTP proxy receives the full address in the request line.
    let proxy = format!("http://{}", e.srv.addr);
    let n = e.m.set_network(net(ProxyMode::Manual, Some(&proxy), Some("puser"), Some("ppass"), Some("Custom/2.0"))).await.unwrap();
    assert_eq!(
        (n.proxy_mode, n.proxy_url.as_deref(), n.proxy_username.as_deref()),
        (ProxyMode::Manual, Some(proxy.as_str()), Some("puser"))
    );
    assert!(n.proxy_has_password);

    let d =
        e.m.create_download(NewDownload { url: "http://files.example.invalid/via-proxy.bin".into(), ..Default::default() }).await.unwrap();
    let d = settle(&e.m, d.id).await;
    assert_eq!(d.status, DownloadStatus::Completed, "{:?}", d.error_detail);
    assert_eq!(std::fs::read(&d.destination).unwrap(), *e.data);
    let h = heads(&e.srv);
    assert!(h.iter().any(|r| r.starts_with("get http://files.example.invalid/via-proxy.bin")), "went through the proxy: {h:?}");
    let expected = format!("proxy-authorization: {}", http_client::basic_authorization("puser", "ppass").to_ascii_lowercase());
    assert!(h.iter().all(|r| r.contains(&expected)), "the proxy is signed in to");
    assert!(h.iter().all(|r| r.contains("user-agent: custom/2.0")), "the chosen user agent is used");

    // After a restart: same settings, password still there (sealed), and not readable in the database.
    restart(&mut e).await;
    let n = e.m.get_network();
    assert_eq!(
        (n.proxy_mode, n.proxy_username.as_deref(), n.proxy_has_password, n.user_agent.as_deref()),
        (ProxyMode::Manual, Some("puser"), true, Some("Custom/2.0"))
    );
    e.m.shutdown().await;
    for f in ["velox.db", "velox.db-wal"] {
        let blob = std::fs::read(e.base.join("state").join(f)).unwrap_or_default();
        assert!(!String::from_utf8_lossy(&blob).contains("ppass"), "{f}: the proxy password is sealed");
    }
}

#[tokio::test]
async fn keeping_clearing_and_refusing_network_settings() {
    let e = env("proxy-rules").await;
    let proxy = format!("http://{}", e.srv.addr);
    e.m.set_network(net(ProxyMode::Manual, Some(&proxy), Some("u"), Some("p"), None)).await.unwrap();
    // No password field sent: the saved one is kept.
    assert!(e.m.set_network(net(ProxyMode::Manual, Some(&proxy), Some("u"), None, None)).await.unwrap().proxy_has_password);
    // An empty one clears it.
    assert!(!e.m.set_network(net(ProxyMode::Manual, Some(&proxy), Some("u"), Some(""), None)).await.unwrap().proxy_has_password);
    // Without a user name there is no password.
    assert!(!e.m.set_network(net(ProxyMode::Manual, Some(&proxy), None, Some("p"), None)).await.unwrap().proxy_has_password);

    let good = e.m.get_network();
    for bad in [
        net(ProxyMode::Manual, None, None, None, None),
        net(ProxyMode::Manual, Some("ftp://p:21"), None, None, None),
        net(ProxyMode::Manual, Some("http://u:pw@p:1"), None, None, None),
        net(ProxyMode::Manual, Some(&proxy), Some("a:b"), None, None),
        net(ProxyMode::System, None, None, None, Some("bad\r\nX: 1")),
    ] {
        assert!(e.m.set_network(bad).await.is_err());
        assert_eq!(e.m.get_network(), good, "a refused change changes nothing");
    }

    // "No proxy": downloads connect directly again; the address is remembered for later.
    let n = e.m.set_network(net(ProxyMode::None, Some(&proxy), None, None, Some(""))).await.unwrap();
    assert_eq!((n.proxy_mode, n.user_agent.as_deref()), (ProxyMode::None, None));
    let d = e.m.create_download(NewDownload { url: e.srv.url("/direct.bin"), ..Default::default() }).await.unwrap();
    assert_eq!(settle(&e.m, d.id).await.status, DownloadStatus::Completed);
    let last = heads(&e.srv).pop().unwrap();
    assert!(last.starts_with("get /direct.bin"), "direct request: {last}");
    assert!(last.contains("user-agent: veloxfetch/"), "default user agent is back");
}

// ============================================================================= speed limits per kind of file

#[tokio::test]
async fn a_speed_limit_for_a_kind_of_file_slows_only_that_kind() {
    let e = env("type-limit").await;
    e.srv.set_data(random_data(1024 * 1024, 5));
    e.m.set_speed_limit(SetSpeedLimit { target: SpeedTarget::Category { name: "video".into() }, bytes_per_sec: Some(256 * 1024) })
        .await
        .unwrap();
    assert_eq!(e.m.speed_limits().by_type.get("video"), Some(&(256 * 1024)));
    assert_eq!(e.m.speed_limits().by_type.len(), 1);

    let t = Instant::now();
    let doc = e.m.create_download(NewDownload { url: e.srv.url("/fast.pdf"), ..Default::default() }).await.unwrap();
    assert_eq!(settle(&e.m, doc.id).await.status, DownloadStatus::Completed);
    let doc_time = t.elapsed();

    let t = Instant::now();
    let video = e.m.create_download(NewDownload { url: e.srv.url("/slow.mp4"), ..Default::default() }).await.unwrap();
    assert_eq!(settle(&e.m, video.id).await.status, DownloadStatus::Completed);
    let video_time = t.elapsed();
    // 1 MiB at 256 KiB/s: about 4 s, minus the bucket's initial burst.
    assert!(video_time >= Duration::from_millis(2000), "the video was limited: {video_time:?}");
    assert!(doc_time < video_time, "the document was not limited ({doc_time:?} vs {video_time:?})");

    e.m.set_speed_limit(SetSpeedLimit { target: SpeedTarget::Category { name: "video".into() }, bytes_per_sec: None }).await.unwrap();
    assert!(e.m.speed_limits().by_type.is_empty());
}

// ============================================================================= show in folder / open

#[tokio::test]
async fn show_in_folder_and_open_use_saved_paths_and_never_open_programs() {
    let e = env("shell").await;
    let get = |path: &'static str| {
        let (m, url) = (e.m.clone(), e.srv.url(path));
        async move {
            let d = m.create_download(NewDownload { url, ..Default::default() }).await.unwrap();
            settle(&m, d.id).await
        }
    };
    let pdf = get("/report.pdf").await;
    let exe = get("/setup.exe").await;
    assert_eq!((pdf.kind, exe.kind), (FileKind::Document, FileKind::Program));

    e.m.show_in_folder(FileTarget::Download { id: pdf.id }).await.unwrap();
    e.m.open_file(FileTarget::Download { id: pdf.id }).await.unwrap();
    e.m.show_in_folder(FileTarget::Download { id: exe.id }).await.unwrap();
    let refused = e.m.open_file(FileTarget::Download { id: exe.id }).await.unwrap_err().to_string();
    assert!(refused.contains("does not open programs"), "{refused}");

    let calls = e.shell.file_calls();
    assert_eq!(
        calls,
        vec![
            ("reveal", PathBuf::from(&pdf.destination)),
            ("open", PathBuf::from(&pdf.destination)),
            ("reveal", PathBuf::from(&exe.destination))
        ]
    );

    // History lines work the same way.
    let hist = e.m.list_history(10).await.unwrap();
    let line = hist.iter().find(|h| h.filename == "report.pdf").unwrap();
    assert!(line.exists && line.kind == FileKind::Document);
    e.m.show_in_folder(FileTarget::History { id: line.id }).await.unwrap();

    // The file was deleted by hand: open says so, show falls back to its folder.
    std::fs::remove_file(&pdf.destination).unwrap();
    assert!(e.m.open_file(FileTarget::Download { id: pdf.id }).await.is_err());
    e.m.show_in_folder(FileTarget::Download { id: pdf.id }).await.unwrap();
    assert_eq!(e.shell.file_calls().last().unwrap().1, PathBuf::from(&pdf.destination).parent().unwrap());
    assert!(!e.m.list_history(10).await.unwrap().iter().find(|h| h.filename == "report.pdf").unwrap().exists);

    // Unknown ids.
    assert!(e.m.show_in_folder(FileTarget::Download { id: 9999 }).await.is_err());
    assert!(e.m.open_file(FileTarget::History { id: 9999 }).await.is_err());
    e.m.open_download_folder().await.unwrap();
}

#[tokio::test]
async fn a_record_pointing_outside_the_download_folders_is_never_shown_or_opened() {
    let e = env("shell-outside").await;
    let d = e.m.create_download(NewDownload { url: e.srv.url("/notes.txt"), ..Default::default() }).await.unwrap();
    let d = settle(&e.m, d.id).await;
    e.m.shutdown().await;
    // Someone edits the database to point the record at a system file.
    let outside = std::env::temp_dir().join("velox-outside-target.txt");
    std::fs::write(&outside, "x").unwrap();
    {
        let store = persistence::SqliteStore::open(e.base.join("state").join("velox.db")).unwrap();
        let mut rec = downloader_core::DownloadStore::load(&store, d.id).unwrap().unwrap();
        rec.destination = outside.clone();
        downloader_core::DownloadStore::save(&store, &rec).unwrap();
    }
    let m = DownloadManager::new(config(&e.base, &e.shell, &e.keys)).await.unwrap();
    let before = e.shell.file_calls().len();
    assert!(m.open_file(FileTarget::Download { id: d.id }).await.is_err());
    assert!(m.show_in_folder(FileTarget::Download { id: d.id }).await.is_err());
    assert_eq!(e.shell.file_calls().len(), before, "nothing was shown or opened");
    let _ = std::fs::remove_file(outside);
}

// ============================================================================= history

#[tokio::test]
async fn history_lines_can_be_removed_without_losing_the_lifetime_total() {
    let e = env("history").await;
    for p in ["/a.bin", "/b.bin", "/c.bin"] {
        let d = e.m.create_download(NewDownload { url: e.srv.url(p), ..Default::default() }).await.unwrap();
        assert_eq!(settle(&e.m, d.id).await.status, DownloadStatus::Completed);
    }
    let total = e.m.get_metrics().await.unwrap().total_downloaded;
    assert_eq!(total, 3 * e.data.len() as u64);
    let lines = e.m.list_history(10).await.unwrap();
    assert_eq!(lines.len(), 3);

    assert_eq!(e.m.forget_history(Some(lines[0].id)).await.unwrap(), 1);
    assert!(e.m.forget_history(Some(lines[0].id)).await.is_err(), "already gone");
    assert_eq!(e.m.list_history(10).await.unwrap().len(), 2);
    assert_eq!(e.m.forget_history(None).await.unwrap(), 2);
    assert!(e.m.list_history(10).await.unwrap().is_empty());
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(e.m.get_metrics().await.unwrap().total_downloaded, total, "the lifetime total is kept");
    // Files are untouched.
    let list = e.m.list_downloads(Default::default()).await.unwrap();
    assert!(list.iter().all(|d| Path::new(&d.destination).is_file()));
}

// ============================================================================= same names

#[tokio::test]
async fn downloads_with_the_same_name_never_share_a_file_or_overwrite_each_other() {
    let e = env("same-name").await;
    // Two different files that both want to be called `setup.exe`, running at the same time.
    let other = random_data(300 * 1024, 999);
    let srv2 = serve(other.clone()).await;
    *e.srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(5);
    *srv2.state.chunk_delay.lock().unwrap() = Duration::from_millis(5);
    let a = e.m.create_download(NewDownload { url: e.srv.url("/setup.exe"), ..Default::default() }).await.unwrap();
    let b = e.m.create_download(NewDownload { url: srv2.url("/setup.exe"), ..Default::default() }).await.unwrap();
    let (a, b) = (settle(&e.m, a.id).await, settle(&e.m, b.id).await);
    assert_eq!((a.status, b.status), (DownloadStatus::Completed, DownloadStatus::Completed), "{:?} {:?}", a.error_detail, b.error_detail);
    assert_ne!(a.destination, b.destination);
    let mut names = [a.filename.clone(), b.filename.clone()];
    names.sort();
    assert_eq!(names, ["setup (1).exe", "setup.exe"]);
    assert_eq!(std::fs::read(&a.destination).unwrap(), *e.data, "each file has its own content");
    assert_eq!(std::fs::read(&b.destination).unwrap(), *other);

    // Once more: the third one is `setup (2).exe`, the first two untouched.
    let c = e.m.create_download(NewDownload { url: e.srv.url("/setup.exe"), ..Default::default() }).await.unwrap();
    let c = settle(&e.m, c.id).await;
    assert_eq!(c.filename, "setup (2).exe");
    assert_eq!(std::fs::read(&b.destination).unwrap(), *other);
}

#[tokio::test]
async fn a_file_that_appears_under_the_same_name_during_the_download_is_not_replaced() {
    let e = env("appears").await;
    *e.srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(40);
    let d = e.m.create_download(NewDownload { url: e.srv.url("/notes.txt"), ..Default::default() }).await.unwrap();
    // Wait until it is writing, then someone saves their own notes.txt in the folder.
    let t = Instant::now();
    let dest = loop {
        let now = e.m.get_download(d.id).await.unwrap();
        if now.downloaded > 0 && now.destination.ends_with("notes.txt") {
            break PathBuf::from(now.destination);
        }
        assert!(t.elapsed() < Duration::from_secs(10));
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    std::fs::write(&dest, b"my own notes").unwrap();
    let done = settle(&e.m, d.id).await;
    assert_eq!(done.status, DownloadStatus::Completed, "{:?}", done.error_detail);
    assert_eq!(std::fs::read(&dest).unwrap(), b"my own notes", "the user's file is untouched");
    assert_eq!(done.filename, "notes (1).txt");
    assert_eq!(std::fs::read(&done.destination).unwrap(), *e.data);
    assert!(!downloader_core::part_path(Path::new(&done.destination)).exists());
    assert!(!downloader_core::part_path(&dest).exists());
}

#[tokio::test]
async fn a_paused_download_keeps_its_name_and_partial_file() {
    let e = env("paused-name").await;
    *e.srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(30);
    let a = e.m.create_download(NewDownload { url: e.srv.url("/movie.mkv"), ..Default::default() }).await.unwrap();
    let t = Instant::now();
    while e.m.get_download(a.id).await.unwrap().downloaded == 0 {
        assert!(t.elapsed() < Duration::from_secs(10));
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let paused = e.m.pause_download(a.id).await.unwrap();
    assert_eq!(paused.status, DownloadStatus::Paused);
    let part = downloader_core::part_path(Path::new(&paused.destination));
    let kept = std::fs::read(&part).unwrap();

    // Another download of the same name while the first one is paused.
    *e.srv.state.chunk_delay.lock().unwrap() = Duration::ZERO;
    let b = e.m.create_download(NewDownload { url: e.srv.url("/movie.mkv"), ..Default::default() }).await.unwrap();
    let b = settle(&e.m, b.id).await;
    assert_eq!(b.filename, "movie (1).mkv", "the paused download's name is taken");
    assert_eq!(std::fs::read(&part).unwrap(), kept, "its partial file was not touched");

    let a = e.m.resume_download(a.id).await.unwrap();
    let a = settle(&e.m, a.id).await;
    assert_eq!((a.status, a.filename.as_str()), (DownloadStatus::Completed, "movie.mkv"));
    assert_eq!(std::fs::read(&a.destination).unwrap(), *e.data);
}
