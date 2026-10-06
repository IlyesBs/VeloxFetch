//! Phase 14, observability: local diagnostics of every download, the application log, and the rule that
//! secrets never reach any of them.
//!
//! * a download's diagnostics: attempts, automatic retries, the last HTTP status, connections, average speed,
//!   the error code and a timeline, kept across restarts and removed with the download;
//! * one log line per run with the download id, status, error code, retries, HTTP status, connections and
//!   average speed; engine lines carry `download{id=N}`;
//! * detailed logging is remembered; the log folder is shown (dry run), never named by the UI;
//! * passwords, cookies, `Authorization` values, proxy passwords and the query values of links appear in none of:
//!   the log (memory and file), the diagnostics, the reports, the database.
//!
//! Each test installs its own log on its own thread (`#[tokio::test]` runs everything on that thread).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use download_manager::{
    BrowserContext, DownloadDto, DownloadManager, ManagerConfig, ManagerError, NewDownload, ProxyMode, SetNetwork, SystemHandle,
};
use downloader_core::{DownloadStatus, RetryPolicy};
use system_integration::DryRunSystem;
use velox_integration_tests::{random_data, serve, Server};

struct Env {
    base: PathBuf,
    m: DownloadManager,
    srv: Server,
    log: diagnostics::LogHandle,
    shell: Arc<DryRunSystem>,
    keys: Arc<secrets::MemoryKeyStore>,
    _guard: tracing::dispatcher::DefaultGuard,
}

fn base_dir(name: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!("velox-diag-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    base
}

fn new_log(base: &Path) -> diagnostics::LogHandle {
    diagnostics::LogHandle::new(diagnostics::LogConfig { dir: Some(base.join("logs")), ..Default::default() })
}

fn config(base: &Path, log: &diagnostics::LogHandle, shell: &Arc<DryRunSystem>, keys: &Arc<secrets::MemoryKeyStore>) -> ManagerConfig {
    let mut c = ManagerConfig::new(base.join("state").join("velox.db"), base.join("downloads"));
    c.progress_interval = Duration::from_millis(40);
    c.download.min_segment_size = 64 * 1024;
    c.download.retry =
        RetryPolicy { max_retries: 2, base_delay: Duration::from_millis(20), max_delay: Duration::from_millis(50), ..Default::default() };
    c.system = SystemHandle(shell.clone());
    c.key_store = keys.clone();
    c.log = Some(log.clone());
    c
}

async fn env(name: &str, size: usize) -> Env {
    let base = base_dir(name);
    let srv = serve(random_data(size, 14)).await;
    let log = new_log(&base);
    let guard = tracing::dispatcher::set_default(&log.dispatch());
    let shell = Arc::new(DryRunSystem::default());
    let keys = Arc::new(secrets::MemoryKeyStore::default());
    let m = DownloadManager::new(config(&base, &log, &shell, &keys)).await.unwrap();
    Env { base, m, srv, log, shell, keys, _guard: guard }
}

async fn settle(m: &DownloadManager, id: i64) -> DownloadDto {
    let t = Instant::now();
    loop {
        let d = m.get_download(id).await.unwrap();
        if matches!(d.status, DownloadStatus::Completed | DownloadStatus::Failed) {
            // The run's diagnostics are written as the task ends, just after the status: let it finish.
            tokio::time::sleep(Duration::from_millis(100)).await;
            return d;
        }
        assert!(t.elapsed() < Duration::from_secs(30), "stuck in {:?}", d.status);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn lines(log: &diagnostics::LogHandle) -> Vec<String> {
    log.recent(1000).into_iter().map(|l| format!("{} {}: {}", l.level, l.target, l.text)).collect()
}

fn line_with<'a>(all: &'a [String], parts: &[&str]) -> Option<&'a String> {
    all.iter().find(|l| parts.iter().all(|p| l.contains(p)))
}

// ============================================================================= numbers and timeline

#[tokio::test]
async fn a_download_reports_status_retries_http_status_connections_and_speed_and_logs_one_line_per_run() {
    let e = env("ok", 1024 * 1024).await;
    // Two data requests answered `503 Service Unavailable` first: the engine retries them.
    e.srv.state.fail_next.store(2, std::sync::atomic::Ordering::SeqCst);
    let d = e.m.create_download(NewDownload { url: e.srv.url("/files/data.bin"), ..Default::default() }).await.unwrap();
    let d = settle(&e.m, d.id).await;
    assert_eq!(d.status, DownloadStatus::Completed, "{:?}", d.error);
    assert_eq!(d.error_code, None);

    let g = e.m.get_download_diagnostics(d.id).await.unwrap();
    assert_eq!(g.status, DownloadStatus::Completed);
    assert_eq!(g.attempts, 1);
    assert!(g.retries >= 2, "the two 503 answers were retried: {g:?}");
    assert_eq!(g.http_status, Some(206), "ranged requests answered 206");
    assert!(g.peak_connections >= 1);
    assert!(g.avg_speed_bps > 0);
    assert!(g.transferred >= 1024 * 1024);
    assert_eq!(g.error_code, None);
    let texts: Vec<&str> = g.events.iter().map(|e| e.text.as_str()).collect();
    assert!(texts[0].starts_with("Started (attempt 1)"), "{texts:?}");
    assert!(texts.iter().any(|t| t.starts_with("Server answered 206: 1.0 MB, parallel download possible")), "{texts:?}");
    assert!(texts.iter().any(|t| t.contains("failed (try 1): HTTP status 503. Trying again")), "{texts:?}");
    assert!(texts.last().unwrap().starts_with("Finished: 1.0 MB received"), "{texts:?}");
    assert!(g.events.iter().any(|e| e.level == "warn"));
    // The detail view of a finished download keeps its average speed (from the saved diagnostics).
    let detail = e.m.get_download_detail(d.id).await.unwrap();
    assert!(detail.avg_speed_bps > 0 && detail.elapsed_secs.is_some(), "{detail:?}");

    // The report: the same numbers, as text.
    for want in [
        "download diagnostics",
        "Status: completed",
        "HTTP status: 206",
        "Attempts: 1 - automatic retries:",
        "mode: parallel",
        "Timeline (UTC):",
    ] {
        assert!(g.report.contains(want), "{want} missing from:\n{}", g.report);
    }

    // The log: one line when the run starts and one when it ends, with every number the roadmap names.
    let all = lines(&e.log);
    let id = format!("id={}", d.id);
    assert!(line_with(&all, &["INFO", "download started", &id, "attempt=1"]).is_some(), "{all:#?}");
    let end = line_with(&all, &["INFO", "download ended", &id]).unwrap_or_else(|| panic!("{all:#?}"));
    for part in ["status=completed", "error_code=-", "retries=", "http_status=206", "connections=", "avg_speed_bps="] {
        assert!(end.contains(part), "{part} missing from {end}");
    }
    // The engine's own warnings carry the download they belong to.
    assert!(
        line_with(&all, &["WARN", &format!("download{{id={}}}: segment failed, retrying", d.id), "HTTP status 503"]).is_some(),
        "{all:#?}"
    );
}

#[tokio::test]
async fn a_failure_has_a_code_and_a_status_that_survive_a_restart_and_go_away_with_the_download() {
    let mut e = env("fail", 64 * 1024).await;
    let d = e.m.create_download(NewDownload { url: e.srv.url("/404/gone.zip"), ..Default::default() }).await.unwrap();
    let d = settle(&e.m, d.id).await;
    assert_eq!(d.status, DownloadStatus::Failed);
    assert_eq!(d.error_code.as_deref(), Some("http_404"));

    let g = e.m.get_download_diagnostics(d.id).await.unwrap();
    assert_eq!((g.error_code.as_deref(), g.http_status, g.attempts), (Some("http_404"), Some(404), 1));
    assert_eq!(g.error.as_deref(), Some("The file is not on the server (the link is broken or has expired)."));
    assert!(g.events.last().unwrap().text.starts_with("Failed (http_404): HTTP status 404"), "{:?}", g.events);
    assert_eq!(g.events.last().unwrap().level, "error");
    let all = lines(&e.log);
    let end = line_with(&all, &["WARN", "download ended", &format!("id={}", d.id)]).unwrap_or_else(|| panic!("{all:#?}"));
    for part in ["status=failed", "error_code=http_404", "http_status=404"] {
        assert!(end.contains(part), "{part} missing from {end}");
    }

    // Retry: a second attempt, counted and told.
    e.m.retry_download(d.id).await.unwrap();
    settle(&e.m, d.id).await;
    let g = e.m.get_download_diagnostics(d.id).await.unwrap();
    assert_eq!(g.attempts, 2);
    assert!(g.events.iter().any(|ev| ev.text == "Retried (attempt 2)"), "{:?}", g.events);

    // Kept in the database: the same after a restart.
    e.m.shutdown().await;
    e.m = DownloadManager::new(config(&e.base, &e.log, &e.shell, &e.keys)).await.unwrap();
    let after = e.m.get_download_diagnostics(d.id).await.unwrap();
    assert_eq!((after.attempts, after.http_status, after.error_code.as_deref()), (2, Some(404), Some("http_404")));
    assert_eq!(after.events, g.events);

    // Removed with the download.
    e.m.delete_download(d.id, false).await.unwrap();
    assert!(matches!(e.m.get_download_diagnostics(d.id).await, Err(ManagerError::NotFound(_))));
    e.m.shutdown().await;
    let store = persistence::SqliteStore::open(e.base.join("state").join("velox.db")).unwrap();
    assert_eq!(store.diagnostics(d.id).unwrap(), None);
    store.integrity_check().unwrap();
}

#[tokio::test]
async fn pausing_and_resuming_counts_attempts_and_continues() {
    let e = env("pause", 2 * 1024 * 1024).await;
    *e.srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(15);
    let d = e.m.create_download(NewDownload { url: e.srv.url("/files/slow.bin"), ..Default::default() }).await.unwrap();
    let t = Instant::now();
    while e.m.get_download(d.id).await.unwrap().downloaded < 128 * 1024 {
        assert!(t.elapsed() < Duration::from_secs(20));
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // While it runs, the live run is included.
    let live = e.m.get_download_diagnostics(d.id).await.unwrap();
    assert_eq!(live.status, DownloadStatus::Downloading);
    assert!(live.transferred > 0 && live.connections >= 1, "{live:?}");
    e.m.pause_download(d.id).await.unwrap();
    *e.srv.state.chunk_delay.lock().unwrap() = Duration::ZERO;
    e.m.resume_download(d.id).await.unwrap();
    let done = settle(&e.m, d.id).await;
    assert_eq!(done.status, DownloadStatus::Completed, "{:?}", done.error);
    let g = e.m.get_download_diagnostics(d.id).await.unwrap();
    assert_eq!(g.attempts, 2);
    let texts: Vec<&str> = g.events.iter().map(|e| e.text.as_str()).collect();
    for want in ["Paused (progress kept)", "Resumed (attempt 2)", "Continuing from"] {
        assert!(texts.iter().any(|t| t.starts_with(want)), "{want} missing from {texts:?}");
    }
    let all = lines(&e.log);
    assert!(line_with(&all, &["download ended", &format!("id={}", d.id), "status=paused"]).is_some(), "{all:#?}");
}

// ============================================================================= the log and its settings

#[tokio::test]
async fn detailed_logging_is_remembered_and_the_log_folder_is_shown_not_named() {
    let mut e = env("settings", 64 * 1024).await;
    let logs = e.m.get_logs(50, false);
    assert!(!logs.detailed && logs.file_logging && !logs.lines.is_empty());
    assert!(logs.lines[0].text.starts_with("VeloxFetch started on "), "{:?}", logs.lines[0]);

    e.m.set_detailed_logging(true).await.unwrap();
    assert!(e.log.detailed());
    // After a restart with a fresh log, the choice comes back from the database.
    e.m.shutdown().await;
    let fresh = new_log(&e.base);
    e.m = DownloadManager::new(config(&e.base, &fresh, &e.shell, &e.keys)).await.unwrap();
    assert!(fresh.detailed() && e.m.get_logs(10, false).detailed);
    e.m.set_detailed_logging(false).await.unwrap();
    assert!(!fresh.detailed());

    // Warnings and errors only.
    fresh.write("WARN", "velox", "a problem");
    fresh.write("INFO", "velox", "not a problem");
    let problems = e.m.get_logs(100, true);
    assert!(problems.lines.iter().all(|l| matches!(l.level, "WARN" | "ERROR")) && problems.lines.iter().any(|l| l.text == "a problem"));

    // "Open log folder": the file manager shows the log file (a dry run here); the path came from Rust.
    e.m.open_log_folder().await.unwrap();
    let calls = e.shell.file_calls();
    assert_eq!(calls.last().unwrap(), &("reveal", e.base.join("logs").join("velox.log")));
    assert!(e.base.join("logs").join("velox.log").is_file());

    // Without a log (the command-line tool, tests): empty answers and clear refusals, never a crash.
    let bare_base = base_dir("bare");
    let mut cfg = config(&bare_base, &fresh, &e.shell, &e.keys);
    cfg.log = None;
    let bare = DownloadManager::new(cfg).await.unwrap();
    assert!(bare.get_logs(10, false).lines.is_empty());
    assert!(bare.set_detailed_logging(true).await.is_err());
    assert!(bare.open_log_folder().await.is_err());
    assert!(bare.diagnostics_report().await.unwrap().contains("log files: no"));
}

// ============================================================================= never log secrets

#[tokio::test]
async fn passwords_cookies_tokens_and_proxy_passwords_never_reach_the_log_the_diagnostics_the_reports_or_the_database() {
    let e = env("secrets", 256 * 1024).await;
    let (b64_ok, b64_bob) = ("dXNlcjpwYXNz", "Ym9iOlBXX1NFQ1JFVF80"); // user:pass, bob:PW_SECRET_4
    let secrets = [
        "COOKIE_SECRET_1",
        "QUERY_SECRET_2",
        "SIG_SECRET_3",
        "PW_SECRET_4",
        b64_bob,
        b64_ok,
        "BROWSER_COOKIE_6",
        "PROXY_SECRET_5",
        "FRAG_SECRET_7",
        "user:pass@",
    ];
    e.log.set_detailed(true); // the most verbose log must be just as clean

    let mut ids = Vec::new();
    // 1. Signed link with credentials in it, a cookie typed in the Add dialog: completes.
    let url = format!("http://user:pass@{}/auth/signed.bin?token=QUERY_SECRET_2&sig=SIG_SECRET_3#FRAG_SECRET_7", e.srv.addr);
    let d = e.m.create_download(NewDownload { url, cookies: Some("sid=COOKIE_SECRET_1".into()), ..Default::default() }).await.unwrap();
    assert_eq!(settle(&e.m, d.id).await.status, DownloadStatus::Completed);
    ids.push(d.id);
    // 2. A wrong password typed in the Add dialog: fails with 401.
    let d =
        e.m.create_download(NewDownload {
            url: e.srv.url("/auth/other.bin?token=QUERY_SECRET_2"),
            username: Some("bob".into()),
            password: Some("PW_SECRET_4".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(settle(&e.m, d.id).await.error_code.as_deref(), Some("http_401"));
    ids.push(d.id);
    // 3. A browser's cookie on a link whose server drops the connection: retried, then fails (the transport
    //    error names the link, query included).
    let d =
        e.m.create_download_from_browser(
            NewDownload { url: e.srv.url("/reset/x.bin?token=QUERY_SECRET_2"), ..Default::default() },
            BrowserContext { referrer: None, cookies: Some("session=BROWSER_COOKIE_6".into()), user_agent: None, incognito: false },
        )
        .await
        .unwrap();
    assert_eq!(settle(&e.m, d.id).await.status, DownloadStatus::Failed);
    ids.push(d.id);
    // 4. A manual proxy with a password, then a download through it (nothing listens there: it fails).
    e.m.set_network(SetNetwork {
        proxy_mode: ProxyMode::Manual,
        proxy_url: Some("http://127.0.0.1:9".into()),
        proxy_username: Some("proxyuser".into()),
        proxy_password: Some("PROXY_SECRET_5".into()),
        user_agent: None,
    })
    .await
    .unwrap();
    let d =
        e.m.create_download(NewDownload { url: e.srv.url("/files/via-proxy.bin?token=QUERY_SECRET_2"), ..Default::default() })
            .await
            .unwrap();
    assert_eq!(settle(&e.m, d.id).await.status, DownloadStatus::Failed);
    ids.push(d.id);

    // Everything diagnostics produce.
    let mut texts = vec![("log ring".to_string(), lines(&e.log).join("\n"))];
    for id in &ids {
        let g = e.m.get_download_diagnostics(*id).await.unwrap();
        texts.push((format!("diagnostics #{id}"), format!("{g:?}")));
        texts.push((format!("report #{id}"), g.report.clone()));
    }
    texts.push(("app report".into(), e.m.diagnostics_report().await.unwrap()));
    texts.push(("get_logs".into(), format!("{:?}", e.m.get_logs(1000, false))));
    e.m.shutdown().await;
    let log_file = e.base.join("logs").join("velox.log");
    texts.push((log_file.display().to_string(), std::fs::read_to_string(&log_file).unwrap()));
    let store = persistence::SqliteStore::open(e.base.join("state").join("velox.db")).unwrap();
    for id in &ids {
        texts.push((format!("saved diagnostics #{id}"), store.diagnostics(*id).unwrap().expect("saved")));
    }
    drop(store);
    for (what, text) in &texts {
        for s in secrets {
            assert!(!text.contains(s), "{s} found in {what}:\n{text}");
        }
    }
    // The database keeps each download's link (it is needed to resume), so only the real secrets are checked
    // in the raw file: they are sealed.
    for f in ["velox.db", "velox.db-wal"] {
        let raw = String::from_utf8_lossy(&std::fs::read(e.base.join("state").join(f)).unwrap_or_default()).into_owned();
        for s in ["COOKIE_SECRET_1", "PW_SECRET_4", b64_bob, b64_ok, "BROWSER_COOKIE_6", "PROXY_SECRET_5", "user:pass@"] {
            assert!(!raw.contains(s), "{s} found in {f}");
        }
    }
    // What is left is still useful: the link without its values, the reason, the retries.
    let log = &texts[0].1;
    assert!(log.contains("/reset/x.bin?token=[redacted]"), "{log}");
    let report = &texts.iter().find(|(w, _)| w == "app report").unwrap().1;
    assert!(report.contains("proxy manual with sign-in") && report.contains("http_401"), "{report}");
    assert!(!report.contains("proxyuser"), "no proxy user name in the report:\n{report}");
}
