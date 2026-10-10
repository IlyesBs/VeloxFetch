//! Smart Duplicate Detection against a real (local) server and a real database: a link the app already has is
//! found in every state (running, paused, finished, failed, only in the history), told with how sure the match is,
//! never confused with another file of the same name, never blocks a wanted copy, never touches an existing file,
//! survives a restart, and never shows a secret of the link.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use download_manager::intelligence::{AnalyzeLink, CheckDuplicate, Confidence, DuplicateDto, DuplicateReason, FileState};
use download_manager::{DownloadDto, DownloadManager, ManagerConfig, NewDownload};
use downloader_core::DownloadStatus;
use sha2::{Digest, Sha256};
use velox_integration_tests::{random_data, serve};

const KB: usize = 1024;

fn paths(name: &str) -> (PathBuf, PathBuf) {
    let base = std::env::temp_dir().join(format!("velox-dup-{}-{name}", std::process::id()));
    (base.join("state").join("velox.db"), base.join("downloads"))
}

async fn open(name: &str) -> DownloadManager {
    let (db, root) = paths(name);
    let mut c = ManagerConfig::new(db, root);
    c.download.min_segment_size = 64 * 1024;
    DownloadManager::new(c).await.unwrap()
}

async fn manager(name: &str) -> DownloadManager {
    let (db, _) = paths(name);
    let _ = std::fs::remove_dir_all(db.parent().unwrap().parent().unwrap());
    open(name).await
}

async fn add(m: &DownloadManager, url: String) -> DownloadDto {
    m.create_download(NewDownload { url, ..Default::default() }).await.unwrap()
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

async fn dup(m: &DownloadManager, url: String) -> Option<DuplicateDto> {
    m.check_duplicate(CheckDuplicate { url, checksum: None }).await.unwrap()
}

#[tokio::test]
async fn a_finished_download_is_found_with_its_file_and_a_new_copy_never_replaces_it() {
    let data = random_data(300 * KB, 1);
    let srv = serve(data.clone()).await;
    let m = manager("finished").await;
    assert_eq!(dup(&m, srv.url("/pkg/app.pdf")).await, None, "nothing yet: no prompt, the download goes as before");

    let first = settle(&m, add(&m, srv.url("/pkg/app.pdf")).await.id).await;
    assert_eq!(first.status, DownloadStatus::Completed);
    let d = dup(&m, srv.url("/pkg/app.pdf")).await.unwrap();
    assert_eq!((d.id, d.status, d.reason), (Some(first.id), Some(DownloadStatus::Completed), DuplicateReason::SameLink));
    assert_eq!((d.confidence, d.changed, d.file, d.openable), (Confidence::Likely, false, Some(FileState::Present), true));
    // Equivalent spellings of the link; another query is another resource.
    let spelled = srv.url("/pkg/app.pdf").replace("http://", "HTTP://");
    assert!(dup(&m, format!("{spelled}#section")).await.is_some(), "scheme case and a #fragment do not change the resource");
    assert_eq!(dup(&m, srv.url("/pkg/app.pdf?v=2")).await, None, "another query: maybe another file");
    assert_eq!(dup(&m, srv.url("/other/app.pdf")).await, None, "same name, another link: not a duplicate");

    // With the server asked (Check link), the same ETag and size prove the same version.
    let a = m.analyze_link(AnalyzeLink { url: Some(srv.url("/pkg/app.pdf")) }).await.unwrap().unwrap();
    let ad = a.duplicate.clone().unwrap();
    assert_eq!((ad.confidence, ad.changed), (Confidence::Exact, false), "{ad:?}");
    let nc = a.name_conflict.clone().expect("its own file has the name");
    assert!(nc.same_resource, "the name is taken by the same resource, not another file: {nc:?}");
    assert_eq!(nc.saved_as.as_deref(), Some("app (1).pdf"));

    // Download again on purpose: a second copy under a new name, the first file untouched.
    let second = settle(&m, add(&m, srv.url("/pkg/app.pdf")).await.id).await;
    assert_eq!(second.status, DownloadStatus::Completed);
    assert_eq!(second.filename, "app (1).pdf");
    assert_ne!(second.destination, first.destination);
    assert_eq!(std::fs::read(&first.destination).unwrap(), *data);

    // The server's file changes: the match stays, but is no longer claimed to be the same version.
    *srv.state.etag.lock().unwrap() = "\"v2\"".into();
    srv.set_data(random_data(200 * KB, 2));
    let a = m.analyze_link(AnalyzeLink { url: Some(srv.url("/pkg/app.pdf")) }).await.unwrap().unwrap();
    let ad = a.duplicate.unwrap();
    assert_eq!((ad.confidence, ad.changed), (Confidence::Possible, true), "{ad:?}");
}

#[tokio::test]
async fn a_gone_or_changed_file_is_never_offered_as_there() {
    let srv = serve(random_data(200 * KB, 3)).await;
    let m = manager("gone").await;
    let done = settle(&m, add(&m, srv.url("/notes.txt")).await.id).await;
    std::fs::write(&done.destination, b"edited by hand").unwrap();
    let d = dup(&m, srv.url("/notes.txt")).await.unwrap();
    assert_eq!((d.file, d.openable), (Some(FileState::Changed), false));
    std::fs::remove_file(&done.destination).unwrap();
    let d = dup(&m, srv.url("/notes.txt")).await.unwrap();
    assert_eq!((d.file, d.openable), (Some(FileState::Missing), false));
}

#[tokio::test]
async fn running_paused_and_failed_downloads_are_told_by_their_state() {
    let srv = serve(random_data(2 * 1024 * KB, 4)).await;
    *srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(20);
    let m = manager("states").await;
    let running = add(&m, srv.url("/big.bin")).await;
    let d = dup(&m, srv.url("/big.bin")).await.unwrap();
    assert_eq!(d.id, Some(running.id));
    assert!(matches!(d.status, Some(DownloadStatus::Queued | DownloadStatus::Downloading)), "{:?}", d.status);
    assert_eq!(d.file, None, "an unfinished download is never taken for a file");

    m.pause_download(running.id).await.unwrap();
    let d = dup(&m, srv.url("/big.bin")).await.unwrap();
    assert_eq!(d.status, Some(DownloadStatus::Paused));
    // Asking changed nothing: still paused, nothing cancelled.
    assert_eq!(m.get_download(running.id).await.unwrap().status, DownloadStatus::Paused);

    let failed = settle(&m, add(&m, srv.url("/e500/broken.bin")).await.id).await;
    assert_eq!(failed.status, DownloadStatus::Failed);
    let d = dup(&m, srv.url("/e500/broken.bin")).await.unwrap();
    assert_eq!((d.status, d.file, d.openable), (Some(DownloadStatus::Failed), None, false));

    // A cancelled one never counts.
    m.cancel_download(running.id).await.unwrap();
    assert_eq!(dup(&m, srv.url("/big.bin")).await, None);
}

#[tokio::test]
async fn the_history_and_a_restart_keep_what_was_downloaded() {
    let srv = serve(random_data(200 * KB, 5)).await;
    let m = manager("restart").await;
    let done = settle(&m, add(&m, srv.url("/kept.zip")).await.id).await;
    // Removed from the list (the file kept): only the history knows it now.
    m.delete_download(done.id, false).await.unwrap();
    let d = dup(&m, srv.url("/kept.zip")).await.unwrap();
    assert_eq!((d.id, d.status, d.file), (None, None, Some(FileState::Present)));
    assert!(d.history_id.is_some());
    m.shutdown().await;
    drop(m);
    let m = open("restart").await;
    let d = dup(&m, srv.url("/kept.zip")).await.unwrap();
    assert_eq!((d.id, d.file), (None, Some(FileState::Present)), "found again after a restart");
}

#[tokio::test]
async fn a_matching_checksum_proves_the_same_content_under_any_link() {
    let data = random_data(200 * KB, 6);
    let sum = format!("sha256:{}", Sha256::digest(data.as_slice()).iter().map(|b| format!("{b:02x}")).collect::<String>());
    let srv = serve(data.clone()).await;
    let m = manager("checksum").await;
    let done =
        m.create_download(NewDownload { url: srv.url("/a/image.iso"), checksum: Some(sum.clone()), ..Default::default() }).await.unwrap();
    assert_eq!(settle(&m, done.id).await.status, DownloadStatus::Completed);
    let mirror = srv.url("/mirror/image.iso");
    let upper = sum.to_uppercase().replace("SHA256", "sha256");
    let d = m.check_duplicate(CheckDuplicate { url: mirror.clone(), checksum: Some(upper) }).await.unwrap().unwrap();
    assert_eq!((d.reason, d.confidence), (DuplicateReason::SameChecksum, Confidence::Exact));
    let other = format!("sha256:{}", "0".repeat(64));
    assert_eq!(m.check_duplicate(CheckDuplicate { url: mirror.clone(), checksum: Some(other) }).await.unwrap(), None);
    // A checksum that does not parse is ignored here (the download itself reports it).
    assert_eq!(m.check_duplicate(CheckDuplicate { url: mirror, checksum: Some("nonsense".into()) }).await.unwrap(), None);
}

#[tokio::test]
async fn another_file_with_the_same_name_is_a_name_conflict_not_a_duplicate() {
    let srv = serve(random_data(200 * KB, 7)).await;
    let m = manager("name").await;
    let done = settle(&m, add(&m, srv.url("/x/report.bin")).await.id).await;
    // Some other file called `notes.bin` in the folder a `notes.bin` download would go to.
    let folder = PathBuf::from(&done.destination).parent().unwrap().to_path_buf();
    std::fs::write(folder.join("notes.bin"), b"mine, not from the internet").unwrap();
    // Another file on the server (another size), so nothing but the name is shared.
    srv.put("/y/notes.bin", random_data(150 * KB, 70).to_vec());
    let a = m.analyze_link(AnalyzeLink { url: Some(srv.url("/y/notes.bin")) }).await.unwrap().unwrap();
    assert_eq!(a.duplicate, None, "the same name alone is never a duplicate");
    let nc = a.name_conflict.expect("the name is taken");
    assert!(!nc.same_resource);
    assert_eq!(nc.saved_as.as_deref(), Some("notes (1).bin"));
    // The download goes under the new name; the user's file is untouched.
    let got = settle(&m, add(&m, srv.url("/y/notes.bin")).await.id).await;
    assert_eq!(got.filename, "notes (1).bin");
    assert_eq!(std::fs::read(folder.join("notes.bin")).unwrap(), b"mine, not from the internet");
    // A free name: no conflict.
    let a = m.analyze_link(AnalyzeLink { url: Some(srv.url("/z/fresh.bin")) }).await.unwrap().unwrap();
    assert_eq!(a.name_conflict, None);
}

#[tokio::test]
async fn identical_requests_at_once_never_share_or_overwrite_a_file() {
    let data = random_data(300 * KB, 8);
    let srv = serve(data.clone()).await;
    *srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(2);
    let m = manager("concurrent").await;
    let url = srv.url("/same.bin");
    let (a, b) = tokio::join!(add(&m, url.clone()), add(&m, url.clone()));
    let (a, b) = (settle(&m, a.id).await, settle(&m, b.id).await);
    assert_eq!((a.status, b.status), (DownloadStatus::Completed, DownloadStatus::Completed));
    assert_ne!(a.destination, b.destination, "two copies wanted on purpose: two files");
    assert_eq!(std::fs::read(&a.destination).unwrap(), *data);
    assert_eq!(std::fs::read(&b.destination).unwrap(), *data);
}

#[tokio::test]
async fn secrets_of_the_link_are_never_part_of_the_answer() {
    let srv = serve(random_data(100 * KB, 9)).await;
    let m = manager("secrets").await;
    let signed = srv.url("/s/file.bin?X-Amz-Signature=topsecret&token=hunter2");
    let with_login = signed.replace("http://", "http://alice:pa55word@");
    let done = settle(&m, add(&m, with_login.clone()).await.id).await;
    assert_eq!(done.status, DownloadStatus::Completed);
    let d = dup(&m, with_login).await.unwrap();
    let json = serde_json::to_string(&d).unwrap();
    for secret in ["topsecret", "hunter2", "alice", "pa55word", "X-Amz", "/s/"] {
        assert!(!json.contains(secret), "{secret} in {json}");
    }
    // Another signature: not matched by the link (a signed link is only proven the same by the file itself).
    assert_eq!(dup(&m, srv.url("/s/file.bin?X-Amz-Signature=other&token=hunter2")).await, None);
}

#[tokio::test]
async fn malformed_links_are_refused_without_touching_anything() {
    let m = manager("malformed").await;
    for bad in ["", "not a link", "file:///etc/passwd", "ftp://x/y"] {
        assert!(m.check_duplicate(CheckDuplicate { url: bad.into(), checksum: None }).await.is_err(), "{bad}");
    }
    assert!(m.list_downloads(Default::default()).await.unwrap().is_empty());
}
