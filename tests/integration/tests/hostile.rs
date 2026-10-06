//! Phase 13.4: security tests where the attacker is the **server** or the **caller**, run end to end
//! through the real download manager on the real file system:
//! server-chosen names (traversal, drive letters, streams, reserved device names, bidi tricks,
//! control characters, huge names), malformed headers, unicode paths, hostile sub-folders and
//! names, archives that try to write outside their folder ("zip slip"), a server that claims a
//! petabyte, and malformed / oversized JSON at the IPC boundary.
//!
//! What every case has to satisfy: the only thing that ever appears is one file **inside** the
//! download folder, with a harmless name and exactly the bytes the server sent.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use download_manager::{DownloadDto, DownloadManager, ManagerConfig, NewDownload};
use downloader_core::{DownloadStatus, RetryPolicy};
use velox_integration_tests::{random_data, serve, Server};

const MB: usize = 1024 * 1024;

struct Env {
    base: PathBuf,
    root: PathBuf,
    m: DownloadManager,
}

async fn env(name: &str) -> Env {
    let base = std::env::temp_dir().join(format!("velox-hostile-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let mut c = ManagerConfig::new(base.join("state").join("velox.db"), base.join("downloads"));
    c.progress_interval = Duration::from_millis(40);
    c.download.min_segment_size = 64 * 1024;
    c.download.retry = RetryPolicy {
        max_retries: 1,
        max_throttle_retries: 1,
        base_delay: Duration::from_millis(5),
        max_delay: Duration::from_millis(20),
        max_retry_after: Duration::from_millis(50),
    };
    let m = DownloadManager::new(c).await.unwrap();
    Env { root: base.join("downloads"), base, m }
}

async fn wait_done(m: &DownloadManager, id: i64) -> DownloadDto {
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

/// Every file and folder below `dir`, relative, sorted.
fn tree(dir: &Path) -> Vec<String> {
    fn walk(d: &Path, prefix: &str, out: &mut Vec<String>) {
        let Ok(rd) = std::fs::read_dir(d) else { return };
        for e in rd.flatten() {
            let name = format!("{prefix}{}", e.file_name().to_string_lossy());
            out.push(name.clone());
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                walk(&e.path(), &format!("{name}/"), out);
            }
        }
    }
    let mut v = Vec::new();
    walk(dir, "", &mut v);
    v.sort();
    v
}

fn files_in(root: &Path) -> Vec<String> {
    std::fs::read_dir(root)
        .map(|rd| rd.flatten().filter(|e| e.path().is_file()).map(|e| e.file_name().to_string_lossy().to_string()).collect())
        .unwrap_or_default()
}

const RESERVED: [&str; 8] = ["CON", "PRN", "AUX", "NUL", "COM1", "COM9", "LPT1", "LPT9"];

/// A name that is safe to create on any of the three systems.
fn assert_harmless_name(name: &str, context: &str) {
    assert!(!name.is_empty() && name != "." && name != "..", "{context}: bad name {name:?}");
    assert!(!name.contains(['/', '\\', ':', '<', '>', '"', '|', '?', '*']), "{context}: separators or illegal characters in {name:?}");
    assert!(!name.chars().any(|c| c.is_control()), "{context}: control character in {name:?}");
    assert!(
        !name.chars().any(|c| matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{200b}'..='\u{200f}' | '\u{feff}')),
        "{context}: invisible or bidi character in {name:?}"
    );
    assert!(!name.ends_with('.') && !name.ends_with(' '), "{context}: Windows would silently change {name:?}");
    let stem = name.split('.').next().unwrap_or("").trim_end().to_ascii_uppercase();
    assert!(!RESERVED.contains(&stem.as_str()), "{context}: reserved device name {name:?}");
    assert!(name.len() <= 255, "{context}: name is {} bytes", name.len());
}

async fn download_with_disposition(header: &str, e: &Env, srv: &Server, data: &[u8]) -> DownloadDto {
    *srv.state.content_disposition.lock().unwrap() = Some(header.to_string());
    let d = e.m.create_download(NewDownload { url: srv.url("/dl/fallback.bin"), ..Default::default() }).await.unwrap();
    let d = wait_done(&e.m, d.id).await;
    assert_eq!(d.status, DownloadStatus::Completed, "{header}: {:?}", d.error);
    let _ = data;
    d
}

// ================================================================== names chosen by the server

#[tokio::test]
async fn a_name_chosen_by_the_server_can_never_leave_the_download_folder_or_be_dangerous() {
    let data = random_data(200 * 1024, 301);
    let srv = serve(data.clone()).await;
    let long = "x".repeat(400);
    let cases: Vec<String> = vec![
        // traversal, absolute and drive paths, both separators
        r#"attachment; filename="../../evil.txt""#.into(),
        r#"attachment; filename="..\..\evil.txt""#.into(),
        r#"attachment; filename="/etc/passwd""#.into(),
        r#"attachment; filename="C:\Windows\System32\evil.dll""#.into(),
        r#"attachment; filename="\\server\share\evil.exe""#.into(),
        r#"attachment; filename="....//....//evil""#.into(),
        "attachment; filename*=UTF-8''%2e%2e%2f%2e%2e%2fevil.txt".into(),
        "attachment; filename*=UTF-8''..%5c..%5cevil.txt".into(),
        // alternate data streams, reserved device names, trailing dots / spaces, dot names
        r#"attachment; filename="report.pdf:evil.exe""#.into(),
        r#"attachment; filename="a.txt::$DATA""#.into(),
        r#"attachment; filename="CON.txt""#.into(),
        r#"attachment; filename="nul""#.into(),
        r#"attachment; filename="com1.log""#.into(),
        r#"attachment; filename="LPT9.tar.gz""#.into(),
        r#"attachment; filename="name.txt.""#.into(),
        r#"attachment; filename="name.txt   ""#.into(),
        r#"attachment; filename="   ""#.into(),
        r#"attachment; filename="...""#.into(),
        r#"attachment; filename="..""#.into(),
        r#"attachment; filename=".""#.into(),
        // illegal characters, leading dash (an option to a naive program), shell metacharacters
        r#"attachment; filename="a<b>c|d?e*f.txt""#.into(),
        r#"attachment; filename="-rf --no-preserve-root""#.into(),
        r#"attachment; filename="$(id) `id` ; rm -rf x & echo pwned.txt""#.into(),
        // control characters, CRLF, escape sequences, NUL, encoded
        "attachment; filename*=UTF-8''%00null.txt".into(),
        "attachment; filename*=UTF-8''line1%0d%0aline2.txt".into(),
        "attachment; filename*=UTF-8''%1b[31mred.txt".into(),
        // bidi override and other invisible characters (the file "fdp.exe" that displays as "exe.pdf")
        "attachment; filename*=UTF-8''%E2%80%AEfdp.exe".into(),
        "attachment; filename*=UTF-8''photo%E2%80%8B.exe".into(),
        // malformed headers
        r#"attachment; filename="unterminated"#.into(),
        "attachment; filename*=UTF-8''%ZZ%GG.txt".into(),
        "attachment; filename*=BOGUS-CHARSET''x.txt".into(),
        "attachment;;;filename==".into(),
        "attachment; filename=".into(),
        r#"attachment; filename="""#.into(),
        "inline".into(),
        format!(r#"attachment; filename="{long}.txt""#),
        format!("attachment; filename*=UTF-8''{}", "%E2%98%83".repeat(200)),
    ];
    for (i, header) in cases.iter().enumerate() {
        let e = env(&format!("names-{i}")).await;
        let d = download_with_disposition(header, &e, &srv, &data).await;
        let context = format!("case {i}: {header}");

        let entries = tree(&e.root);
        assert_eq!(entries.len(), 1, "{context}: expected exactly one file in the download folder, found {entries:?}");
        assert_harmless_name(&entries[0], &context);
        assert_eq!(files_in(&e.root).len(), 1, "{context}");
        eprintln!("{:<75} -> {:?}", header.chars().take(75).collect::<String>(), entries[0]);
        assert_eq!(std::fs::read(e.root.join(&entries[0])).unwrap(), *data, "{context}: content");
        // Nothing appeared beside the download folder (a traversal would have put it there).
        let beside = tree(&e.base);
        assert!(
            beside.iter().all(|p| p == "downloads" || p == &format!("downloads/{}", entries[0]) || p.starts_with("state")),
            "{context}: stray files {beside:?}"
        );
        assert!(d.destination.contains("downloads") || d.destination.is_empty(), "{context}: {}", d.destination);
    }
}

#[tokio::test]
async fn unicode_in_server_names_is_kept_when_it_is_harmless() {
    let data = random_data(64 * 1024, 302);
    let srv = serve(data.clone()).await;
    let e = env("unicode-name").await;
    // "héllo_世界_😀.bin"
    download_with_disposition("attachment; filename*=UTF-8''h%C3%A9llo_%E4%B8%96%E7%95%8C_%F0%9F%98%80.bin", &e, &srv, &data).await;
    let files = files_in(&e.root);
    assert_eq!(files, ["h\u{e9}llo_\u{4e16}\u{754c}_\u{1f600}.bin"], "a normal unicode name is not mangled");
    assert_eq!(std::fs::read(e.root.join(&files[0])).unwrap(), *data);
}

// ================================================================== unicode paths and hostile input from the caller

#[tokio::test]
async fn unicode_folders_work_and_hostile_folders_and_names_are_refused() {
    let data = random_data(64 * 1024, 303);
    let srv = serve(data.clone()).await;
    let e = env("caller").await;

    // Unicode is fine: accents, CJK, emoji, right-to-left scripts.
    let d =
        e.m.create_download(NewDownload {
            url: srv.url("/u.bin"),
            subdir: Some("T\u{e9}l\u{e9}chargements/\u{65e5}\u{672c}\u{8a9e} \u{1f600}/\u{5e2}\u{5d1}\u{5e8}\u{5d9}\u{5ea}".into()),
            filename: Some("na\u{ef}ve caf\u{e9} \u{2014} \u{fc}n\u{ef}c\u{f6}d\u{e9} \u{2603}.bin".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let d = wait_done(&e.m, d.id).await;
    assert_eq!(d.status, DownloadStatus::Completed, "{:?}", d.error);
    assert_eq!(std::fs::read(&d.destination).unwrap(), *data, "a unicode path is a normal path");

    // Hostile sub-folders: refused (or, at worst, kept inside the folder) and nothing is created outside.
    let subdirs = [
        "../x",
        "..\\x",
        "a/../../x",
        "/abs/x",
        "C:\\x",
        "C:/x",
        "\\\\server\\share",
        "x\0y",
        "CON",
        "a/nul/b",
        "..",
        ".",
        "a/./b/../../..",
        "x:stream",
        "trailing.",
        " leading",
        "a\u{202e}b",
        "a\r\nb",
        "\u{ff0e}\u{ff0e}/x",
        "%2e%2e/x",
        "..%2fx",
        &"d/".repeat(300),
    ];
    for s in subdirs {
        let r = e.m.create_download(NewDownload { url: srv.url("/s.bin"), subdir: Some(s.to_string()), ..Default::default() }).await;
        if let Ok(d) = r {
            let d = wait_done(&e.m, d.id).await;
            let dest = PathBuf::from(&d.destination);
            let inside = dest.canonicalize().map(|p| p.starts_with(e.root.canonicalize().unwrap())).unwrap_or(true);
            assert!(inside, "sub-folder {s:?} put the file at {}", d.destination);
        }
    }
    // Hostile names: the UI-chosen name is checked strictly, never "repaired".
    let names = [
        "../x",
        "..\\x",
        "a/b",
        "/etc/passwd",
        "C:\\x",
        "x:y",
        "CON",
        "nul.txt",
        "x\0y",
        "a\u{202e}b",
        "trailing.",
        "..",
        "",
        "   ",
        "a|b",
        "-rf",
    ];
    for n in names {
        let r = e.m.create_download(NewDownload { url: srv.url("/n.bin"), filename: Some(n.to_string()), ..Default::default() }).await;
        if n == "-rf" {
            continue; // legal on every system: a leading dash is only a problem for programs that parse options; none is run
        }
        assert!(r.is_err(), "the name {n:?} should have been refused");
    }
    let outside: Vec<String> = tree(&e.base).into_iter().filter(|p| !p.starts_with("downloads") && !p.starts_with("state")).collect();
    assert!(outside.is_empty(), "files appeared outside the download folder: {outside:?}");
}

// ================================================================== malicious archives

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

/// A valid ZIP whose single entry is named `name` (stored, no compression).
fn zip_with(name: &str, content: &[u8]) -> Vec<u8> {
    let crc = crc32(content);
    let mut z = Vec::new();
    let local = z.len() as u32;
    z.extend([0x50, 0x4b, 3, 4, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    z.extend(crc.to_le_bytes());
    z.extend((content.len() as u32).to_le_bytes());
    z.extend((content.len() as u32).to_le_bytes());
    z.extend((name.len() as u16).to_le_bytes());
    z.extend([0, 0]);
    z.extend(name.as_bytes());
    z.extend(content);
    let cd = z.len() as u32;
    z.extend([0x50, 0x4b, 1, 2, 20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    z.extend(crc.to_le_bytes());
    z.extend((content.len() as u32).to_le_bytes());
    z.extend((content.len() as u32).to_le_bytes());
    z.extend((name.len() as u16).to_le_bytes());
    z.extend([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    z.extend(local.to_le_bytes());
    z.extend(name.as_bytes());
    let cd_len = z.len() as u32 - cd;
    z.extend([0x50, 0x4b, 5, 6, 0, 0, 0, 0, 1, 0, 1, 0]);
    z.extend(cd_len.to_le_bytes());
    z.extend(cd.to_le_bytes());
    z.extend([0, 0]);
    z
}

/// A POSIX tar with one entry named `name`.
fn tar_with(name: &str, content: &[u8]) -> Vec<u8> {
    let mut h = [0u8; 512];
    h[..name.len()].copy_from_slice(name.as_bytes());
    h[100..107].copy_from_slice(b"0000644");
    h[108..115].copy_from_slice(b"0000000");
    h[116..123].copy_from_slice(b"0000000");
    h[124..135].copy_from_slice(format!("{:011o}", content.len()).as_bytes());
    h[136..147].copy_from_slice(b"00000000000");
    h[156] = b'0';
    h[257..262].copy_from_slice(b"ustar");
    h[148..156].copy_from_slice(b"        ");
    let sum: u32 = h.iter().map(|&b| b as u32).sum();
    h[148..155].copy_from_slice(format!("{sum:06o}\0").as_bytes());
    let mut t = h.to_vec();
    t.extend(content);
    t.resize(t.len().div_ceil(512) * 512 + 1024, 0);
    t
}

#[tokio::test]
async fn an_archive_that_tries_to_write_outside_its_folder_is_only_ever_saved_as_a_file() {
    // VeloxFetch never opens, extracts or previews what it downloads, so "zip slip" cannot happen:
    // this proves it end to end (and `policy.test.mjs` proves no archive library is even linked).
    for (kind, bytes) in [
        ("zip", zip_with("../../../evil.txt", b"pwned")),
        ("zip-abs", zip_with("C:\\Windows\\evil.txt", b"pwned")),
        ("tar", tar_with("../../evil.txt", b"pwned")),
    ] {
        let srv = serve(Arc::new(bytes.clone())).await;
        let e = env(&format!("archive-{kind}")).await;
        let d = e.m.create_download(NewDownload { url: srv.url(&format!("/bad.{kind}")), ..Default::default() }).await.unwrap();
        let d = wait_done(&e.m, d.id).await;
        assert_eq!(d.status, DownloadStatus::Completed, "{kind}: {:?}", d.error);
        assert_eq!(std::fs::read(&d.destination).unwrap(), bytes, "{kind}: saved untouched");
        let all = tree(&e.base);
        assert!(!all.iter().any(|p| p.contains("evil")), "{kind}: something was extracted: {all:?}");
        assert_eq!(files_in(&e.root).len(), 1, "{kind}");
    }
}

// ================================================================== oversized payloads

#[tokio::test]
async fn a_server_claiming_a_petabyte_is_refused_before_any_space_is_reserved() {
    let srv = serve(random_data(MB, 304)).await;
    let e = env("petabyte").await;
    let d = e.m.create_download(NewDownload { url: srv.url("/huge/f.bin"), ..Default::default() }).await.unwrap();
    let t = Instant::now();
    let d = wait_done(&e.m, d.id).await;
    assert!(t.elapsed() < Duration::from_secs(10), "took {:?}", t.elapsed());
    assert_eq!(d.status, DownloadStatus::Failed);
    let msg = d.error.unwrap_or_default();
    assert!(msg.contains("Not enough free space") && msg.contains("PB"), "the reason should be shown plainly: {msg}");
    assert!(d.error_detail.unwrap_or_default().contains("free disk space"), "and the technical one kept");
    assert!(tree(&e.root).is_empty(), "a huge .part file was left behind: {:?}", tree(&e.root));
}

// ================================================================== invalid JSON at the IPC boundary

#[test]
fn malformed_oversized_and_hostile_json_never_gets_past_the_ipc_schema() {
    let deep = format!("{}1{}", "[".repeat(5000), "]".repeat(5000));
    let bad: Vec<String> = vec![
        "".into(),
        "{".into(),
        "}".into(),
        "null".into(),
        "[]".into(),
        "42".into(),
        r#""just a string""#.into(),
        r#"{"url": }"#.into(),
        r#"{"url":"http://a/b","unknown":1}"#.into(),
        r#"{"url":42}"#.into(),
        r#"{"url":["http://a/b"]}"#.into(),
        r#"{"url":{"$ref":"x"}}"#.into(),
        r#"{"url":"http://a/b","priority":"ultra"}"#.into(),
        r#"{"url":"http://a/b","priority":-1}"#.into(),
        r#"{"url":"http://a/b","start":"yes"}"#.into(),
        r#"{"url":"http://a/b","speedLimit":-5}"#.into(),
        r#"{"url":"http://a/b","speedLimit":1e999}"#.into(),
        r#"{"url":"http://a/b","speedLimit":18446744073709551616}"#.into(),
        r#"{"url":"http://a/b","__proto__":{"admin":true}}"#.into(),
        r#"{"url":"http://a/b","constructor":{"prototype":{}}}"#.into(),
        "{\"url\":\"http://a/b\"\u{0}}".into(),
        r#"{"url":"http://a/b",}"#.into(),
        r#"{'url':'http://a/b'}"#.into(),
        r#"{"url":"http://a/b"} {"url":"http://c/d"}"#.into(),
        deep.clone(),
        format!(r#"{{"url":{deep}}}"#),
    ];
    for doc in &bad {
        let shown: String = doc.chars().take(60).collect();
        let r = std::panic::catch_unwind(|| serde_json::from_str::<NewDownload>(doc));
        assert!(r.is_ok(), "parsing panicked on {shown:?}");
        assert!(r.unwrap().is_err(), "accepted: {shown:?}");
    }
    // The same for the other structures the UI sends.
    for doc in &bad {
        let r = std::panic::catch_unwind(|| {
            let _ = serde_json::from_str::<download_manager::SetSpeedLimit>(doc);
            let _ = serde_json::from_str::<download_manager::BrowserContext>(doc);
            let _ = serde_json::from_str::<download_manager::ListFilter>(doc);
        });
        assert!(r.is_ok(), "a parser panicked");
    }
    // Schema-valid but absurd (a 2 MiB link) is caught one step later, by the business validation.
    let big = serde_json::from_str::<NewDownload>(&format!(r#"{{"url":"http://a/{}"}}"#, "a".repeat(2 * MB))).unwrap();
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let refused = rt.block_on(async {
        let e = env("bigurl").await;
        e.m.create_download(big).await.is_err()
    });
    assert!(refused, "a 2 MiB link must be refused");
    // A well-formed one still works, so the loop above is testing something real.
    let ok = serde_json::from_str::<NewDownload>(r#"{"url":"http://a/b","priority":"high","start":false}"#).unwrap();
    assert_eq!(ok.url, "http://a/b");
}

/// A server that answers a file link with a web page (a sign-in or "file not available" page, as
/// Google Drive does for a private file) must not leave that page on disk under the file's name.
#[tokio::test]
async fn a_web_page_sent_instead_of_the_expected_file_is_refused() {
    let e = env("webpage").await;
    let srv = serve(Arc::new(b"<!doctype html><title>Sign in</title>".to_vec())).await;

    // The name the browser (or the user) expected, or the one in the link.
    for (url, filename) in [(srv.url("/html/x"), Some("report.pdf")), (srv.url("/html/archive.zip"), None)] {
        let d = e.m.create_download(NewDownload { url, filename: filename.map(str::to_string), ..Default::default() }).await.unwrap();
        let d = wait_done(&e.m, d.id).await;
        assert_eq!(d.status, DownloadStatus::Failed, "{d:?}");
        assert_eq!(d.error_code.as_deref(), Some("web_page"), "{d:?}");
        assert!(d.error.as_deref().unwrap_or("").contains("web page instead of the file"), "{d:?}");
    }
    assert!(tree(&e.root).iter().all(|f| !f.ends_with(".pdf") && !f.ends_with(".zip")), "{:?}", tree(&e.root));

    // A real web page is still downloaded, and a server that names its attachment is trusted.
    let page = e.m.create_download(NewDownload { url: srv.url("/html/page.html"), ..Default::default() }).await.unwrap();
    assert_eq!(wait_done(&e.m, page.id).await.status, DownloadStatus::Completed);
    *srv.state.content_disposition.lock().unwrap() = Some("attachment; filename=\"named.zip\"".into());
    let named = e.m.create_download(NewDownload { url: srv.url("/html/y"), ..Default::default() }).await.unwrap();
    let named = wait_done(&e.m, named.id).await;
    assert_eq!(named.status, DownloadStatus::Completed, "{named:?}");
    assert_eq!(named.filename, "named.zip");
    let _ = std::fs::remove_dir_all(&e.base);
}
