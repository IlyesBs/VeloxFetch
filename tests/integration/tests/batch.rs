//! Phase 8: batch downloads (a list of links, a pattern), duplicate detection against the download
//! list, folder validation and the hard limits, through the real manager.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use download_manager::{BatchRequest, DownloadManager, ItemState, ListFilter, ManagerConfig, NewDownload};
use downloader_core::DownloadStatus;
use velox_integration_tests::{random_data, serve};

fn root(name: &str) -> (PathBuf, PathBuf) {
    let base = std::env::temp_dir().join(format!("velox-batch-{}-{name}", std::process::id()));
    (base.join("state").join("velox.db"), base.join("downloads"))
}

async fn manager(name: &str) -> DownloadManager {
    let (db, dl) = root(name);
    let _ = std::fs::remove_dir_all(db.parent().unwrap().parent().unwrap());
    let mut c = ManagerConfig::new(db, dl);
    c.max_concurrent_downloads = 4;
    DownloadManager::new(c).await.unwrap()
}

async fn until<F: std::future::Future<Output = bool>>(what: &str, mut f: impl FnMut() -> F) {
    let t = Instant::now();
    while !f().await {
        assert!(t.elapsed() < Duration::from_secs(30), "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn count(m: &DownloadManager) -> usize {
    m.list_downloads(ListFilter::default()).await.unwrap().len()
}

fn pattern(base: &str, p: &str) -> BatchRequest {
    BatchRequest { pattern: Some(format!("{base}/{p}")), ..Default::default() }
}

#[tokio::test]
async fn a_pattern_downloads_every_file_into_its_folder() {
    let srv = serve(random_data(100_000, 1)).await;
    let m = manager("pattern").await;
    let req = BatchRequest { subdir: Some("series".into()), ..pattern(&srv.url(""), "file_[001-012].bin") };

    let preview = m.preview_batch(req.clone()).await.unwrap();
    assert_eq!((preview.total, preview.ok, preview.duplicates, preview.existing, preview.invalid), (12, 12, 0, 0, 0));
    assert_eq!(preview.items[0].filename, "file_001.bin");
    assert_eq!(count(&m).await, 0, "a preview adds nothing");

    let r = m.add_batch(req).await.unwrap();
    assert_eq!((r.added, r.failed), (12, 0));
    until("all 12 to finish", || async {
        let all = m.list_downloads(ListFilter::default()).await.unwrap();
        all.len() == 12 && all.iter().all(|d| d.status == DownloadStatus::Completed)
    })
    .await;
    let all = m.list_downloads(ListFilter::default()).await.unwrap();
    for d in &all {
        assert!(d.destination.contains("series"), "{}", d.destination);
        assert!(PathBuf::from(&d.destination).is_file());
    }
    let mut names: Vec<_> = all.iter().map(|d| d.filename.clone()).collect();
    names.sort();
    assert_eq!(names[0], "file_001.bin");
    assert_eq!(names[11], "file_012.bin");
}

#[tokio::test]
async fn links_already_in_the_list_and_repeated_links_are_skipped() {
    let srv = serve(random_data(10_000, 2)).await;
    let m = manager("dupes").await;
    let already = m.create_download(NewDownload { url: srv.url("/a.bin"), start: Some(false), ..Default::default() }).await.unwrap();

    let text = format!(
        "# comment\n{a}\n{b}\n{b}#frag\n\n{b}\nnot a link\nftp://x.test/f\n{c}\n",
        a = srv.url("/a.bin"),
        b = srv.url("/b.bin"),
        c = srv.url("/c.bin")
    );
    let req = BatchRequest { links: Some(text), start: Some(false), ..Default::default() };
    let p = m.preview_batch(req.clone()).await.unwrap();
    assert_eq!((p.total, p.ok, p.duplicates, p.existing, p.invalid), (7, 2, 2, 1, 2));
    // Problems are listed first.
    assert!(matches!(p.items[0].state, ItemState::AlreadyInList | ItemState::DuplicateInBatch | ItemState::Invalid { .. }));
    assert!(p.items.iter().any(|i| i.line == 2 && i.state == ItemState::AlreadyInList), "line numbers are the original ones");

    let r = m.add_batch(req.clone()).await.unwrap();
    assert_eq!((r.added, r.duplicates, r.existing, r.invalid), (2, 2, 1, 2));
    assert_eq!(count(&m).await, 3, "the old one plus b and c");

    // Doing it again adds nothing: everything is in the list now.
    let again = m.add_batch(req.clone()).await.unwrap();
    assert_eq!(again.added, 0);
    assert_eq!(count(&m).await, 3);

    // A cancelled download does not block adding its link again.
    m.cancel_download(already.id).await.unwrap();
    let again = m.add_batch(BatchRequest { links: Some(srv.url("/a.bin")), start: Some(false), ..Default::default() }).await.unwrap();
    assert_eq!(again.added, 1);
}

#[tokio::test]
async fn limits_and_bad_input_fail_the_whole_batch_and_add_nothing() {
    let srv = serve(random_data(1000, 3)).await;
    let m = manager("limits").await;
    let base = srv.url("");
    let over = pattern(&base, "f[1-1001].bin");
    let one_million = pattern(&base, "f[1-100][1-100][1-100].bin");
    let text_too_many: String = (0..1001).map(|i| format!("{base}/f{i}.bin\n")).collect();
    let huge_text = "x".repeat(600 * 1024);
    let bad_requests = [
        over,
        one_million,
        BatchRequest { links: Some(text_too_many), ..Default::default() },
        BatchRequest { links: Some(huge_text), ..Default::default() },
        BatchRequest { links: Some("   \n# only a comment\n".into()), ..Default::default() },
        BatchRequest::default(),
        BatchRequest { links: Some(srv.url("/a.bin")), pattern: Some(format!("{base}/[1-2]")), ..Default::default() },
        BatchRequest { subdir: Some("..".into()), ..pattern(&base, "f[1-2].bin") },
        BatchRequest { subdir: Some("..\\..\\evil".into()), ..pattern(&base, "f[1-2].bin") },
        BatchRequest { subdir: Some("C:\\Windows".into()), ..pattern(&base, "f[1-2].bin") },
        BatchRequest { subdir: Some("/etc".into()), ..pattern(&base, "f[1-2].bin") },
        BatchRequest { subdir: Some("CON".into()), ..pattern(&base, "f[1-2].bin") },
        BatchRequest { subdir: Some("a/../../b".into()), ..pattern(&base, "f[1-2].bin") },
        pattern(&base, "no-range.bin"),
        pattern(&base, "f[5-1].bin"),
    ];
    for (i, req) in bad_requests.into_iter().enumerate() {
        assert!(m.preview_batch(req.clone()).await.is_err(), "preview {i}");
        assert!(m.add_batch(req).await.is_err(), "add {i}");
    }
    assert_eq!(count(&m).await, 0, "a refused batch adds nothing at all");

    // The boundary itself works.
    let ok = m.add_batch(BatchRequest { start: Some(false), ..pattern(&base, "f[1-1000].bin") }).await.unwrap();
    assert_eq!(ok.added, 1000);
    assert_eq!(count(&m).await, 1000);
}

#[tokio::test]
async fn hostile_links_in_a_list_are_reported_not_downloaded() {
    let srv = serve(random_data(1000, 4)).await;
    let m = manager("hostile").await;
    let text = format!(
        "javascript:alert(1)\nfile:///C:/Windows/win.ini\ndata:text/plain,hi\nhttp://\nhttps://\n{}\n\"quoted\"\nhttps://a.test/x y\n",
        srv.url("/fine.bin")
    );
    let r = m.add_batch(BatchRequest { links: Some(text), start: Some(false), ..Default::default() }).await.unwrap();
    assert_eq!((r.added, r.invalid), (1, 7));
    let all = m.list_downloads(ListFilter::default()).await.unwrap();
    assert_eq!(all.len(), 1);
    assert!(all[0].url.ends_with("/fine.bin"));
}

#[tokio::test]
async fn clipboard_links_already_in_the_list_are_not_offered_again() {
    let srv = serve(random_data(1000, 5)).await;
    let m = manager("clip").await;
    let url = srv.url("/movie.mp4");
    assert_eq!(m.new_download_links(&url).await, vec![url.clone()]);
    assert!(m.new_download_links("hello there").await.is_empty());
    m.create_download(NewDownload { url: url.clone(), start: Some(false), ..Default::default() }).await.unwrap();
    assert!(m.new_download_links(&url).await.is_empty(), "already downloading or downloaded");
    let other = srv.url("/other.zip");
    assert_eq!(
        m.new_download_links(&format!(
            "{url}
{other}"
        ))
        .await,
        vec![other],
        "only the new one is offered"
    );
}

#[tokio::test]
async fn clipboard_detection_is_off_by_default_and_the_choice_is_remembered() {
    let m = manager("clipset").await;
    assert!(!m.clipboard_watch(), "reading the clipboard is opt-in");
    m.set_clipboard_watch(true).await.unwrap();
    assert!(m.clipboard_watch());
    m.shutdown().await;
    let (db, dl) = root("clipset");
    let again = DownloadManager::new(ManagerConfig::new(db, dl)).await.unwrap();
    assert!(again.clipboard_watch(), "kept across restarts");
    again.set_clipboard_watch(false).await.unwrap();
    assert!(!again.clipboard_watch());
}
