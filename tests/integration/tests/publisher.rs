//! The publisher's checksum (feature #6): the checksum list a site publishes next to its files (`SHA256SUMS`,
//! `<file>.sha256`...) is found while the file downloads, and the file is checked against it before it is finished.
//! A file that does not match is never kept; one with no list is finished as before; the user's own checksum wins.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use download_manager::intelligence::AnalyzeLink;
use download_manager::{DownloadDto, DownloadManager, ManagerConfig, NewDownload};
use downloader_core::{AdaptiveConfig, DownloadStatus, RetryPolicy};
use sha2::{Digest, Sha256, Sha512};
use velox_integration_tests::{random_data, serve, Server};

const MB: usize = 1024 * 1024;

struct Env {
    root: PathBuf,
    db: PathBuf,
}

fn env(name: &str) -> Env {
    let base = std::env::temp_dir().join(format!("velox-pub-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    Env { root: base.join("downloads"), db: base.join("state").join("velox.db") }
}

async fn manager_with(e: &Env, over_http: bool) -> DownloadManager {
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
    c.recovery_max_tries = 0;
    // The test server is plain http (a release build looks next to https links only).
    c.publisher_over_http = over_http;
    DownloadManager::new(c).await.unwrap()
}

async fn manager(e: &Env) -> DownloadManager {
    manager_with(e, true).await
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn sha256(d: &[u8]) -> String {
    hex(&Sha256::digest(d))
}

async fn wait_end(m: &DownloadManager, id: i64) -> DownloadDto {
    let t = Instant::now();
    loop {
        let d = m.get_download(id).await.unwrap();
        if matches!(d.status, DownloadStatus::Completed | DownloadStatus::Failed | DownloadStatus::Cancelled) {
            return d;
        }
        assert!(t.elapsed() < Duration::from_secs(30), "download {id} stuck in {:?}", d.status);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn download(m: &DownloadManager, srv: &Server, path: &str) -> DownloadDto {
    let d = m.create_download(NewDownload { url: srv.url(path), ..Default::default() }).await.unwrap();
    wait_end(m, d.id).await
}

fn asked(srv: &Server, name: &str) -> Vec<String> {
    srv.state
        .request_heads
        .lock()
        .unwrap()
        .iter()
        .filter(|h| h.lines().next().is_some_and(|l| l.contains(&name.to_ascii_lowercase())))
        .cloned()
        .collect()
}

async fn timeline(m: &DownloadManager, id: i64) -> Vec<String> {
    m.get_download_diagnostics(id).await.unwrap().events.into_iter().map(|e| e.text).collect()
}

#[tokio::test]
async fn a_file_that_matches_the_publishers_list_is_verified_and_says_so_everywhere() {
    let data = random_data(3 * MB, 61);
    let srv = serve(data.clone()).await;
    srv.put("/pub/SHA256SUMS", format!("{}  other.iso\n{}  debian-12.iso\n", "a".repeat(64), sha256(&data)));
    let e = env("match");
    let m = manager(&e).await;
    let d = download(&m, &srv, "/pub/debian-12.iso").await;
    assert_eq!(d.status, DownloadStatus::Completed, "{:?}", d.error);
    let v = d.verify.expect("verified");
    assert_eq!(
        (v.state.as_str(), v.by.as_deref(), v.algo.as_deref(), v.list.as_deref()),
        ("verified", Some("publisher"), Some("SHA-256"), Some("SHA256SUMS"))
    );
    let tl = timeline(&m, d.id).await;
    assert!(tl.iter().any(|t| t == "Checking the file against the publisher's checksum (SHA256SUMS)"), "{tl:?}");
    assert!(tl.iter().any(|t| t == "The file matches the publisher's checksum (SHA256SUMS)"), "{tl:?}");
    let h = m.list_history(10).await.unwrap();
    let hv = h[0].verify.as_ref().expect("the history line says it too");
    assert_eq!((hv.by.as_deref(), hv.list.as_deref()), (Some("publisher"), Some("SHA256SUMS")));
    let detail = m.get_download_detail(d.id).await.unwrap();
    assert_eq!(detail.checksum, Some(format!("sha256:{}", sha256(&data))));
    m.shutdown().await;
    drop(m);
    // Kept after a restart.
    let m = manager(&e).await;
    let again = m.get_download(d.id).await.unwrap().verify.expect("still verified");
    assert_eq!((again.state.as_str(), again.list.as_deref()), ("verified", Some("SHA256SUMS")));
    assert!(m.list_history(10).await.unwrap()[0].verify.is_some());
}

#[tokio::test]
async fn a_damaged_file_is_never_kept() {
    let data = random_data(2 * MB, 62);
    let srv = serve(data.clone()).await;
    srv.put("/pub/SHA256SUMS", format!("{}  app.zip\n", "0".repeat(64)));
    let e = env("damaged");
    let m = manager(&e).await;
    let d = download(&m, &srv, "/pub/app.zip").await;
    assert_eq!(d.status, DownloadStatus::Failed);
    assert_eq!(d.error_code.as_deref(), Some("publisher_checksum_mismatch"), "{:?}", d.error);
    assert!(d.error.as_deref().unwrap().contains("publisher"), "{:?}", d.error);
    assert!(d.verify.is_none());
    assert!(!e.root.join("app.zip").exists() && !e.root.join("app.zip.part").exists(), "nothing of it is left");
    assert!(m.list_history(10).await.unwrap().is_empty(), "not in the history");
    let doctor = d.doctor.expect("the Doctor explains it");
    assert_eq!(doctor.code, "checksum_mismatch");
    // Retry downloads it again: a corrected list now matches.
    srv.put("/pub/SHA256SUMS", format!("{}  app.zip\n", sha256(&data)));
    // The answer is kept a few minutes for the same link: a fresh manager asks again.
    m.shutdown().await;
    drop(m);
    let m = manager(&e).await;
    m.retry_download(d.id).await.unwrap();
    let d = wait_end(&m, d.id).await;
    assert_eq!(d.status, DownloadStatus::Completed, "{:?}", d.error);
    assert_eq!(d.verify.unwrap().state, "verified");
}

#[tokio::test]
async fn every_usual_list_is_understood() {
    let data = random_data(MB, 63);
    let srv = serve(data.clone()).await;
    // A list made for this one file: a lone digest.
    srv.put("/a/f1.bin.sha256", format!("{}\n", sha256(&data)));
    // SHA-512 in BSD form.
    srv.put("/b/SHA512SUMS", format!("SHA512 (f2.bin) = {}\n", hex(&Sha512::digest(&data[..]))));
    // Node's name; a clearsigned list (the signature is not checked).
    srv.put(
        "/c/SHASUMS256.txt",
        format!("-----BEGIN PGP SIGNED MESSAGE-----\nHash: SHA256\n\n{}  f3.bin\n-----BEGIN PGP SIGNATURE-----\nxx\n", sha256(&data)),
    );
    let e = env("formats");
    let m = manager(&e).await;
    for (path, list, algo) in
        [("/a/f1.bin", "f1.bin.sha256", "SHA-256"), ("/b/f2.bin", "SHA512SUMS", "SHA-512"), ("/c/f3.bin", "SHASUMS256.txt", "SHA-256")]
    {
        let d = download(&m, &srv, path).await;
        let v = d.verify.unwrap_or_else(|| panic!("{path} not verified: {:?}", d.error));
        assert_eq!((v.state.as_str(), v.list.as_deref(), v.algo.as_deref()), ("verified", Some(list), Some(algo)), "{path}");
    }
}

#[tokio::test]
async fn no_list_too_large_a_list_or_an_ambiguous_one_finishes_as_before() {
    let data = random_data(MB, 64);
    let srv = serve(data.clone()).await;
    // Larger than a list can be (the right line at its very end).
    let mut big = format!("{}  x\n", "b".repeat(64)).repeat(16_000);
    big.push_str(&format!("{}  big.bin\n", sha256(&data)));
    srv.put("/big/SHA256SUMS", big);
    // The same name twice with two digests: never guessed.
    srv.put("/two/SHA256SUMS", format!("{}  two.bin\n{}  two.bin\n", sha256(&data), "c".repeat(64)));
    let e = env("none");
    let m = manager(&e).await;
    for path in ["/none/f.bin", "/big/big.bin", "/two/two.bin"] {
        let t = Instant::now();
        let d = download(&m, &srv, path).await;
        assert_eq!(d.status, DownloadStatus::Completed, "{path}: {:?}", d.error);
        assert!(d.verify.is_none(), "{path}: {:?}", d.verify);
        assert!(t.elapsed() < Duration::from_secs(10), "{path}: no list never holds a download up");
        assert!(!timeline(&m, d.id).await.iter().any(|l| l.contains("checksum")));
    }
    // The other names served the file itself (this server answers every path): not a list either.
    assert!(!asked(&srv, "/none/SHA256SUMS").is_empty());
}

#[tokio::test]
async fn a_slow_list_never_holds_the_download_up_for_long() {
    let data = random_data(MB, 65);
    let srv = serve(data.clone()).await;
    srv.put("/slow/SHA256SUMS", format!("{}  f.bin\n", sha256(&data)));
    *srv.state.route_delay.lock().unwrap() = Duration::from_secs(30);
    let e = env("slow");
    let m = manager(&e).await;
    let t = Instant::now();
    let d = download(&m, &srv, "/slow/f.bin").await;
    assert_eq!(d.status, DownloadStatus::Completed, "{:?}", d.error);
    assert!(d.verify.is_none());
    assert!(t.elapsed() < Duration::from_secs(15), "{:?}", t.elapsed());
}

#[tokio::test]
async fn the_users_own_checksum_wins_and_the_setting_turns_it_off() {
    let data = random_data(MB, 66);
    let srv = serve(data.clone()).await;
    srv.put("/u/SHA256SUMS", format!("{}  f.bin\n{}  g.bin\n", "0".repeat(64), "0".repeat(64)));
    let e = env("user");
    let m = manager(&e).await;
    // Given by the user: the publisher's (wrong) list is not even asked.
    let d = m
        .create_download(NewDownload {
            url: srv.url("/u/f.bin"),
            checksum: Some(format!("sha256:{}", sha256(&data))),
            ..Default::default()
        })
        .await
        .unwrap();
    let d = wait_end(&m, d.id).await;
    assert_eq!(d.status, DownloadStatus::Completed, "{:?}", d.error);
    let v = d.verify.unwrap();
    assert_eq!((v.by.as_deref(), v.list), (Some("you"), None));
    assert!(asked(&srv, "/u/sha256sums").is_empty());
    assert!(timeline(&m, d.id).await.iter().any(|t| t == "The file matches the checksum you gave"));
    assert_eq!(m.list_history(1).await.unwrap()[0].verify.as_ref().unwrap().by.as_deref(), Some("you"));
    // Turned off: nothing is asked and the file finishes as before.
    assert!(m.publisher_check(), "on by default");
    assert!(!m.set_publisher_check(false).await.unwrap());
    let d = download(&m, &srv, "/u/g.bin").await;
    assert_eq!(d.status, DownloadStatus::Completed, "{:?}", d.error);
    assert!(d.verify.is_none());
    assert!(asked(&srv, "/u/sha256sums").is_empty() && asked(&srv, "/u/g.bin.sha256").is_empty());
    m.set_publisher_check(true).await.unwrap();
    assert!(m.publisher_check());
}

#[tokio::test]
async fn lists_are_asked_without_cookies_or_sign_in_and_only_over_https_in_a_release() {
    let data = random_data(MB, 67);
    let srv = serve(data.clone()).await;
    srv.put("/c/SHA256SUMS", format!("{}  f.bin\n", sha256(&data)));
    let e = env("cookies");
    let m = manager(&e).await;
    let d = m
        .create_download(NewDownload {
            url: srv.url("/c/f.bin"),
            cookies: Some("session=SECRET".into()),
            referrer: Some("https://site.example/page".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let d = wait_end(&m, d.id).await;
    assert_eq!(d.verify.unwrap().state, "verified");
    let lists = asked(&srv, "/c/sha256sums");
    assert!(!lists.is_empty());
    assert!(
        lists.iter().all(|h| !h.contains("secret") && !h.contains("cookie:") && !h.contains("referer:") && !h.contains("authorization:")),
        "{lists:?}"
    );
    // A release build (no test switch): a plain http link is never looked next to.
    let e2 = env("release");
    let m2 = manager_with(&e2, false).await;
    let d = download(&m2, &srv, "/c/f.bin").await;
    assert_eq!(d.status, DownloadStatus::Completed);
    assert!(d.verify.is_none());
}

#[tokio::test]
async fn the_add_dialog_says_beforehand_and_the_download_reuses_the_answer() {
    let data = random_data(MB, 68);
    let srv = serve(data.clone()).await;
    srv.put("/i/SHA256SUMS", format!("{}  f.bin\n", sha256(&data)));
    let e = env("analysis");
    let m = manager(&e).await;
    let a = m.analyze_link(AnalyzeLink { url: Some(srv.url("/i/f.bin")) }).await.unwrap().unwrap();
    let p = a.publisher_checksum.expect("found beforehand");
    assert_eq!((p.list.as_str(), p.algo.as_str()), ("SHA256SUMS", "SHA-256"));
    let before = asked(&srv, "/i/sha256sums").len();
    let d = download(&m, &srv, "/i/f.bin").await;
    assert_eq!(d.verify.unwrap().state, "verified");
    assert_eq!(asked(&srv, "/i/sha256sums").len(), before, "the answer of a moment ago is reused");
    let none = m.analyze_link(AnalyzeLink { url: Some(srv.url("/i2/f.bin")) }).await.unwrap().unwrap();
    assert!(none.publisher_checksum.is_none());
}

#[tokio::test]
async fn while_it_downloads_the_card_says_it_will_be_verified() {
    let data = random_data(4 * MB, 69);
    let srv = serve(data.clone()).await;
    srv.put("/w/SHA256SUMS", format!("{}  f.bin\n", sha256(&data)));
    *srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(30);
    let e = env("live");
    let m = manager(&e).await;
    let d = m.create_download(NewDownload { url: srv.url("/w/f.bin"), ..Default::default() }).await.unwrap();
    let t = Instant::now();
    let v = loop {
        let now = m.get_download(d.id).await.unwrap();
        if let Some(v) = now.verify.filter(|v| v.state == "ready") {
            assert!(matches!(now.status, DownloadStatus::Queued | DownloadStatus::Downloading), "{:?}", now.status);
            break v;
        }
        assert!(t.elapsed() < Duration::from_secs(10), "never said it will be verified");
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!((v.by.as_deref(), v.list.as_deref()), (Some("publisher"), Some("SHA256SUMS")));
    *srv.state.chunk_delay.lock().unwrap() = Duration::ZERO;
    assert_eq!(wait_end(&m, d.id).await.verify.unwrap().state, "verified");
}

#[tokio::test]
async fn a_batch_from_one_folder_asks_its_lists_once_and_a_site_without_lists_only_a_few_times() {
    let data = random_data(256 * 1024, 70);
    let srv = serve(data.clone()).await;
    let names: Vec<String> = (1..=6).map(|i| format!("part{i}.bin")).collect();
    srv.put("/batch/SHA256SUMS", names.iter().map(|n| format!("{}  {n}\n", sha256(&data))).collect::<String>());
    let e = env("batch");
    let m = manager(&e).await;
    let mut ids = Vec::new();
    for n in &names {
        ids.push(m.create_download(NewDownload { url: srv.url(&format!("/batch/{n}")), ..Default::default() }).await.unwrap().id);
    }
    for id in &ids {
        let d = wait_end(&m, *id).await;
        assert_eq!(d.verify.map(|v| v.state), Some("verified".to_string()), "{:?}", d.error);
    }
    assert_eq!(asked(&srv, "/batch/sha256sums ").len(), 1, "the folder's list is asked once for the whole batch");
    assert!(asked(&srv, ".bin.sha256").is_empty(), "the shared list named them all: no file's own list asked");
    // A site with no list at all: its files' own lists are asked for the first two files only.
    let ids: Vec<i64> = {
        let mut out = Vec::new();
        for i in 1..=6 {
            out.push(m.create_download(NewDownload { url: srv.url(&format!("/plain/f{i}.bin")), ..Default::default() }).await.unwrap().id);
        }
        out
    };
    for id in &ids {
        assert!(wait_end(&m, *id).await.verify.is_none());
    }
    assert_eq!(asked(&srv, "/plain/sha256sums ").len(), 1);
    let own: Vec<String> =
        srv.state.request_heads.lock().unwrap().iter().filter(|h| h.contains("/plain/f") && h.contains(".bin.sha")).cloned().collect();
    assert!(own.len() <= 2 * 4, "{} requests for the files' own lists", own.len());
}

#[tokio::test]
async fn a_server_that_refuses_the_file_is_never_asked_for_lists() {
    let data = random_data(MB, 71);
    let srv = serve(data.clone()).await;
    // A signed link that expired: the server answers 403.
    srv.state.expired.lock().unwrap().insert("old".to_string());
    let e = env("refused");
    let m = manager(&e).await;
    let d = download(&m, &srv, "/signed/old/f.bin").await;
    assert_eq!(d.status, DownloadStatus::Failed, "{:?}", d.status);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        asked(&srv, "/gone/sha256sums").is_empty() && asked(&srv, "/gone/f.bin.sha").is_empty(),
        "the lists wait for the transfer to begin"
    );
}
