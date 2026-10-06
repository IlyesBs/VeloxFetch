//! Phase 10: filesystem security. Hostile names, escapes through links, planted files.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use download_manager::{DownloadManager, ManagerConfig, NewDownload};
use downloader_core::{DownloadStatus, RetryPolicy};
use velox_integration_tests::{random_data, serve, Server};

struct Env {
    base: PathBuf,
    root: PathBuf,
    outside: PathBuf,
}

fn env(name: &str) -> Env {
    let base = std::env::temp_dir().join(format!("velox-p10-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let outside = base.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    Env { root: base.join("downloads"), outside, base }
}

async fn manager(e: &Env) -> DownloadManager {
    let mut c = ManagerConfig::new(e.base.join("state").join("velox.db"), &e.root);
    c.progress_interval = Duration::from_millis(40);
    c.download.checkpoint_interval = Duration::from_millis(50);
    c.download.retry = RetryPolicy {
        max_retries: 1,
        max_throttle_retries: 1,
        base_delay: Duration::from_millis(5),
        max_delay: Duration::from_millis(20),
        max_retry_after: Duration::from_millis(50),
    };
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

fn link_dir(link: &Path, target: &Path) -> bool {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link).is_ok()
    }
    #[cfg(windows)]
    {
        // A junction needs no privilege on Windows.
        std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J", link.to_str().unwrap(), target.to_str().unwrap()])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}

fn link_file(link: &Path, target: &Path) -> bool {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link).is_ok()
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_file(target, link).is_ok()
    }
}

fn names_in(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> =
        std::fs::read_dir(dir).map(|r| r.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect()).unwrap_or_default();
    v.sort();
    v
}

fn dl(srv: &Server, path: &str) -> NewDownload {
    NewDownload { url: srv.url(path), ..Default::default() }
}

#[tokio::test]
async fn hostile_names_from_the_server_end_up_as_plain_files_in_the_download_folder() {
    let e = env("names");
    let m = manager(&e).await;
    let srv = serve(random_data(2000, 1)).await;
    let long = format!("{}.bin", "a".repeat(300));
    let cases: Vec<(String, Option<String>)> = vec![
        ("/..%2F..%2Fevil.bin".into(), Some("evil.bin".into())),
        ("/..%5C..%5Cwin.bin".into(), Some("win.bin".into())),
        ("/CON.txt".into(), Some("_CON.txt".into())),
        ("/nul".into(), Some("_nul".into())),
        ("/a%3Ab%2Ac.bin".into(), Some("a_b_c.bin".into())),
        ("/report.txt%3Aevil.exe".into(), Some("report.txt_evil.exe".into())),
        ("/x%E2%80%AEfdp.exe".into(), Some("xfdp.exe".into())),
        ("/trail.%20.".into(), Some("trail".into())),
        (format!("/{long}"), None),
    ];
    for (i, (path, want)) in cases.iter().enumerate() {
        // Each on its own path so the file names cannot collide.
        let id = m.create_download(dl(&srv, &format!("{path}?n={i}"))).await.unwrap().id;
        let d = wait_final(&m, id).await;
        assert_eq!(d.status, DownloadStatus::Completed, "{path}: {:?}", d.error);
        match want {
            Some(w) => assert_eq!(&d.filename, w, "{path}"),
            None => assert!(d.filename.len() <= 200 && d.filename.ends_with(".bin"), "{}", d.filename),
        }
        assert!(e.root.join(&d.filename).is_file(), "{path}: not directly inside the download folder");
    }
    // Nothing was written anywhere else: the base folder holds only what the app made itself.
    assert_eq!(names_in(&e.base), vec!["downloads", "outside", "state"]);
    assert!(names_in(&e.outside).is_empty());
    assert!(names_in(&e.root).iter().all(|n| !n.ends_with(".part")), "no partial files left behind");
    m.shutdown().await;
}

#[tokio::test]
async fn names_and_folders_chosen_in_the_ui_are_rejected_not_repaired() {
    let e = env("ui");
    let m = manager(&e).await;
    let srv = serve(random_data(100, 2)).await;
    for bad in ["a\u{202e}b.exe", "..", "nul", "x.", "a:b", "a/b", "a\\b", "tab\tname", "zero\u{200b}width", " lead"] {
        let r = m.create_download(NewDownload { filename: Some(bad.into()), ..dl(&srv, "/f.bin") }).await;
        assert!(r.is_err(), "file name {bad:?} must be rejected");
    }
    let too_long = "é".repeat(150); // 300 bytes
    assert!(m.create_download(NewDownload { filename: Some(too_long), ..dl(&srv, "/f.bin") }).await.is_err());
    for bad in ["..", "../x", "a/../../x", "/abs", "C:\\x", "a:b", "CON", "x./y", "a\u{202e}b", "d\u{200b}"] {
        let r = m.create_download(NewDownload { subdir: Some(bad.into()), ..dl(&srv, "/f.bin") }).await;
        assert!(r.is_err(), "sub-folder {bad:?} must be rejected");
    }
    assert_eq!(names_in(&e.root), Vec::<String>::new(), "rejected requests create nothing");
    m.shutdown().await;
}

#[tokio::test]
async fn a_folder_swapped_for_a_link_after_queueing_cannot_redirect_the_download() {
    let e = env("swap");
    let m = manager(&e).await;
    let srv = serve(random_data(5000, 3)).await;
    let queued = m.create_download(NewDownload { subdir: Some("sub".into()), start: Some(false), ..dl(&srv, "/data.bin") }).await.unwrap();
    // The check at creation time passed; now the folder becomes a link to somewhere else.
    let link = e.root.join("sub");
    if !link_dir(&link, &e.outside) {
        eprintln!("skipped: cannot create a folder link here");
        m.shutdown().await;
        return;
    }
    m.start_download(queued.id).await.unwrap();
    let d = wait_final(&m, queued.id).await;
    assert_eq!(d.status, DownloadStatus::Failed, "must not download through the link");
    assert!(d.error.unwrap_or_default().contains("escapes"), "the reason is stated");
    assert!(names_in(&e.outside).is_empty(), "nothing may be written outside: {:?}", names_in(&e.outside));
    m.shutdown().await;
}

#[tokio::test]
async fn a_planted_link_in_place_of_the_partial_file_is_never_written_through() {
    let e = env("planted");
    let m = manager(&e).await;
    let srv = serve(random_data(300 * 1024, 4)).await;
    std::fs::create_dir_all(&e.root).unwrap();
    let victim = e.outside.join("victim.txt");
    std::fs::write(&victim, b"precious data").unwrap();
    let part = e.root.join("planted.bin.part");
    if !link_file(&part, &victim) {
        eprintln!("skipped: cannot create a file symlink here");
        m.shutdown().await;
        return;
    }
    let id = m.create_download(NewDownload { filename: Some("planted.bin".into()), ..dl(&srv, "/x.bin") }).await.unwrap().id;
    let d = wait_final(&m, id).await;
    assert_eq!(d.status, DownloadStatus::Failed, "{:?}", d.error);
    assert_eq!(std::fs::read(&victim).unwrap(), b"precious data", "the link target must be untouched");
    assert!(!e.root.join("planted.bin").exists());
    m.shutdown().await;
}

#[tokio::test]
async fn deleting_a_download_never_deletes_outside_the_folder() {
    let e = env("delete");
    let m = manager(&e).await;
    let srv = serve(random_data(1000, 5)).await;
    let id = m.create_download(dl(&srv, "/keep.bin")).await.unwrap().id;
    assert_eq!(wait_final(&m, id).await.status, DownloadStatus::Completed);
    // Replace the finished file by a link to a file outside; deleting the entry (with its file)
    // may remove the link but never the file it pointed to.
    let victim = e.outside.join("victim.txt");
    std::fs::write(&victim, b"precious data").unwrap();
    let file = e.root.join("keep.bin");
    std::fs::remove_file(&file).unwrap();
    if link_file(&file, &victim) {
        m.delete_download(id, true).await.unwrap();
        assert_eq!(std::fs::read(&victim).unwrap(), b"precious data");
    } else {
        eprintln!("skipped: cannot create a file symlink here");
    }
    m.shutdown().await;
}

#[tokio::test]
async fn normal_downloads_and_sub_folders_still_work() {
    let e = env("normal");
    let m = manager(&e).await;
    let srv = serve(random_data(400 * 1024, 6)).await;
    let a = m
        .create_download(NewDownload { subdir: Some("movies/2026".into()), filename: Some("évènement ✓.bin".into()), ..dl(&srv, "/x") })
        .await
        .unwrap()
        .id;
    let d = wait_final(&m, a).await;
    assert_eq!(d.status, DownloadStatus::Completed, "{:?}", d.error);
    assert!(e.root.join("movies").join("2026").join("évènement ✓.bin").is_file());
    m.shutdown().await;
}

#[cfg(windows)]
fn zone_of(file: &Path) -> Option<String> {
    let mut s = file.as_os_str().to_owned();
    s.push(":Zone.Identifier");
    std::fs::read_to_string(s).ok()
}

#[cfg(windows)]
#[tokio::test]
async fn finished_downloads_are_marked_as_coming_from_the_internet_unless_switched_off() {
    let e = env("motw");
    let m = manager(&e).await;
    let srv = serve(random_data(3000, 7)).await;
    let id = m.create_download(dl(&srv, "/tool.exe?sig=SECRET-TOKEN")).await.unwrap().id;
    let d = wait_final(&m, id).await;
    assert_eq!(d.status, DownloadStatus::Completed);
    match zone_of(&e.root.join("tool.exe")) {
        Some(z) => {
            assert!(z.contains("ZoneId=3"), "{z}");
            assert!(z.contains("HostUrl=http://127.0.0.1") && !z.contains("SECRET-TOKEN"), "the origin is recorded without its query: {z}");
        }
        None => eprintln!("skipped: this file system has no alternate data streams"),
    }
    m.shutdown().await;

    // The switch: an explicit opt-out is honoured.
    let e2 = env("motw-off");
    let mut c = ManagerConfig::new(e2.base.join("state").join("velox.db"), &e2.root);
    c.download.mark_of_the_web = false;
    let m = DownloadManager::new(c).await.unwrap();
    let id = m.create_download(dl(&srv, "/plain.bin")).await.unwrap().id;
    assert_eq!(wait_final(&m, id).await.status, DownloadStatus::Completed);
    assert!(zone_of(&e2.root.join("plain.bin")).is_none());
    m.shutdown().await;
}
