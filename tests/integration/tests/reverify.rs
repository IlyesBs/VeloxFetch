//! Segment re-verification and source-change detection on resume: saved bytes are checked before they are trusted
//! again, and bytes of another version of the file never join the saved ones.

use std::io::{Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use download_manager::{DownloadDto, DownloadManager, ManagerConfig, NewDownload};
use downloader_core::{part_path, AdaptiveConfig, DownloadStatus, DownloadStore, RetryPolicy};
use persistence::SqliteStore;
use velox_integration_tests::{random_data, serve, Server};

const MB: usize = 1024 * 1024;

struct Env {
    root: PathBuf,
    db: PathBuf,
}

impl Drop for Env {
    // The test's folder goes away with it (the temp folder may be in memory).
    fn drop(&mut self) {
        if let Some(base) = self.root.parent() {
            let _ = std::fs::remove_dir_all(base);
        }
    }
}

fn env(name: &str) -> Env {
    let base = std::env::temp_dir().join(format!("velox-reverify-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    Env { root: base.join("downloads"), db: base.join("state").join("velox.db") }
}

async fn manager(e: &Env) -> DownloadManager {
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
    DownloadManager::new(c).await.unwrap()
}

async fn wait_status(m: &DownloadManager, id: i64, want: DownloadStatus) -> DownloadDto {
    let t = Instant::now();
    loop {
        let d = m.get_download(id).await.unwrap();
        if d.status == want {
            return d;
        }
        assert!(t.elapsed() < Duration::from_secs(30), "download {id} stuck in {:?}, wanted {want:?} ({:?})", d.status, d.error);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A download of `path` paused cleanly part-way (some segments complete, others not). The manager is shut down.
async fn paused_part_way(e: &Env, srv: &Server, path: &str) -> i64 {
    let m = manager(e).await;
    *srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(15);
    let d = m.create_download(NewDownload { url: srv.url(path), ..Default::default() }).await.unwrap();
    let t = Instant::now();
    while m.get_download(d.id).await.unwrap().downloaded < 2 * MB as u64 {
        assert!(t.elapsed() < Duration::from_secs(20), "never got going");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    m.pause_download(d.id).await.unwrap();
    wait_status(&m, d.id, DownloadStatus::Paused).await;
    // The tests need finished parts next to unfinished ones: on a loaded machine, go on a little until there are.
    for _ in 0..50 {
        let segs = m.get_download_detail(d.id).await.unwrap().segments;
        if segs.iter().any(|s| s.status == downloader_core::SegmentStatus::Completed) {
            break;
        }
        m.resume_download(d.id).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        m.pause_download(d.id).await.unwrap();
        wait_status(&m, d.id, DownloadStatus::Paused).await;
    }
    m.shutdown().await;
    *srv.state.chunk_delay.lock().unwrap() = Duration::ZERO;
    d.id
}

async fn diagnostics(m: &DownloadManager, id: i64) -> String {
    format!("{:?}", m.get_download_diagnostics(id).await.unwrap())
}

#[tokio::test]
async fn a_clean_pause_resumes_without_reading_the_saved_bytes_again() {
    let data = random_data(6 * MB, 91);
    let srv = serve(data.clone()).await;
    let e = env("clean");
    let id = paused_part_way(&e, &srv, "/clean.bin").await;
    let store = SqliteStore::open(&e.db).unwrap();
    let rec = store.load(id).unwrap().unwrap();
    let done: Vec<_> = rec.segments.iter().filter(|s| s.is_complete()).collect();
    assert!(!done.is_empty(), "some segments completed");
    assert!(done.iter().all(|s| s.hash.as_deref().is_some_and(|h| h.starts_with("sha256:"))), "every completed segment is hashed");
    assert!(rec.segments.iter().filter(|s| !s.is_complete()).all(|s| s.hash.is_none()), "never a hash for an incomplete one");
    assert!(rec.part_stamp.is_some());
    drop(store);

    let m = manager(&e).await;
    let served = srv.state.bytes_served.load(Ordering::SeqCst);
    m.resume_download(id).await.unwrap();
    wait_status(&m, id, DownloadStatus::Completed).await;
    assert!(std::fs::read(e.root.join("clean.bin")).unwrap() == *data);
    assert!(srv.state.bytes_served.load(Ordering::SeqCst) - served < data.len() as u64 - rec.downloaded + MB as u64);
    assert!(!diagnostics(&m, id).await.contains("checked again"), "untouched: nothing re-read");
}

#[tokio::test]
async fn a_locally_changed_segment_is_found_and_only_its_range_is_downloaded_again() {
    let data = random_data(6 * MB, 92);
    let srv = serve(data.clone()).await;
    let e = env("changed");
    let id = paused_part_way(&e, &srv, "/changed.bin").await;
    let store = SqliteStore::open(&e.db).unwrap();
    let rec = store.load(id).unwrap().unwrap();
    let victim = rec.segments.iter().find(|s| s.is_complete()).unwrap().clone();
    drop(store);
    // Another program (or a disk fault) changes one byte inside a finished part.
    std::thread::sleep(Duration::from_millis(20));
    let mut f = std::fs::OpenOptions::new().write(true).open(part_path(&rec.destination)).unwrap();
    f.seek(SeekFrom::Start(victim.start + 10)).unwrap();
    f.write_all(&[data[victim.start as usize + 10] ^ 0xff]).unwrap();
    drop(f);

    let m = manager(&e).await;
    let served = srv.state.bytes_served.load(Ordering::SeqCst);
    m.resume_download(id).await.unwrap();
    wait_status(&m, id, DownloadStatus::Completed).await;
    assert!(std::fs::read(e.root.join("changed.bin")).unwrap() == *data, "byte for byte: the changed part was fetched again");
    let fetched = srv.state.bytes_served.load(Ordering::SeqCst) - served;
    assert!(
        fetched < data.len() as u64 - rec.downloaded + victim.len() + MB as u64,
        "only the missing parts and the changed one: {fetched}"
    );
    assert!(diagnostics(&m, id).await.contains("no longer match their check"), "said in the timeline");
}

#[tokio::test]
async fn after_a_crash_the_saved_segments_are_checked_and_intact_ones_kept() {
    let data = random_data(6 * MB, 93);
    let srv = serve(data.clone()).await;
    let e = env("crash");
    let id = paused_part_way(&e, &srv, "/crash.bin").await;
    // What a crash leaves: the last saved stamp is not the file's (more was written after it).
    let store = SqliteStore::open(&e.db).unwrap();
    let mut rec = store.load(id).unwrap().unwrap();
    rec.part_stamp = Some("0:0".into());
    store.save(&rec).unwrap();
    drop(store);

    let m = manager(&e).await;
    let served = srv.state.bytes_served.load(Ordering::SeqCst);
    m.resume_download(id).await.unwrap();
    wait_status(&m, id, DownloadStatus::Completed).await;
    assert!(std::fs::read(e.root.join("crash.bin")).unwrap() == *data);
    assert!(srv.state.bytes_served.load(Ordering::SeqCst) - served < data.len() as u64 - rec.downloaded + MB as u64, "intact parts kept");
    assert!(diagnostics(&m, id).await.contains("all intact"));
}

#[tokio::test]
async fn segments_saved_by_an_older_version_still_resume() {
    let data = random_data(6 * MB, 94);
    let srv = serve(data.clone()).await;
    let e = env("legacy");
    let id = paused_part_way(&e, &srv, "/legacy.bin").await;
    let store = SqliteStore::open(&e.db).unwrap();
    let mut rec = store.load(id).unwrap().unwrap();
    for s in &mut rec.segments {
        s.hash = None;
    }
    rec.part_stamp = None;
    store.save(&rec).unwrap();
    drop(store);

    let m = manager(&e).await;
    m.resume_download(id).await.unwrap();
    wait_status(&m, id, DownloadStatus::Completed).await;
    assert!(std::fs::read(e.root.join("legacy.bin")).unwrap() == *data);
    let diag = diagnostics(&m, id).await;
    assert!(diag.contains("older parts could not be checked"), "the limit is said, not hidden: {diag}");
}

#[tokio::test]
async fn a_file_changed_on_the_server_mid_download_is_never_mixed_in() {
    // Same size, new content and a new ETag, from a server that ignores If-Range: only the answer's ETag tells.
    let (old, new) = (random_data(6 * MB, 95), random_data(6 * MB, 96));
    let srv = serve(old.clone()).await;
    let e = env("source");
    let m = manager(&e).await;
    *srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(15);
    let d = m.create_download(NewDownload { url: srv.url("/source.bin"), ..Default::default() }).await.unwrap();
    while m.get_download(d.id).await.unwrap().downloaded < MB as u64 {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    srv.set_data(new.clone());
    *srv.state.etag.lock().unwrap() = "\"v2\"".into();
    let failed = wait_status(&m, d.id, DownloadStatus::Failed).await;
    assert_eq!(failed.error_code.as_deref(), Some("source_changed"), "{:?}", failed.error);
    let doctor = failed.doctor.expect("diagnosed");
    assert_eq!(doctor.code, "source_changed");
    assert!(!e.root.join("source.bin").exists(), "never finished from mixed bytes");

    // Retry: the server describes another version, so the saved part is not continued: it starts over.
    *srv.state.chunk_delay.lock().unwrap() = Duration::ZERO;
    m.retry_download(d.id).await.unwrap();
    wait_status(&m, d.id, DownloadStatus::Completed).await;
    assert!(std::fs::read(e.root.join("source.bin")).unwrap() == *new, "entirely the new version");
    assert!(diagnostics(&m, d.id).await.contains("starting over"));
}

#[tokio::test]
async fn another_total_size_in_a_ranged_answer_stops_the_download() {
    let data = random_data(6 * MB, 97);
    let srv = serve(data.clone()).await;
    let e = env("total");
    let m = manager(&e).await;
    *srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(15);
    let d = m.create_download(NewDownload { url: srv.url("/total.bin"), ..Default::default() }).await.unwrap();
    while m.get_download(d.id).await.unwrap().downloaded < MB as u64 {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    srv.set_data(random_data(7 * MB, 97)); // same ETag, another size
    let failed = wait_status(&m, d.id, DownloadStatus::Failed).await;
    assert_eq!(failed.error_code.as_deref(), Some("source_changed"), "{:?}", failed.error);
    assert!(failed.downloaded > 0, "the saved part is kept until a safe decision");
}

#[tokio::test]
async fn without_any_validator_a_resume_starts_over_instead_of_trusting() {
    let data = random_data(6 * MB, 98);
    let srv = serve(data.clone()).await;
    let e = env("novalidator");
    let id = paused_part_way(&e, &srv, "/noetag/nv.bin").await;
    let m = manager(&e).await;
    m.resume_download(id).await.unwrap();
    wait_status(&m, id, DownloadStatus::Completed).await;
    assert!(std::fs::read(e.root.join("nv.bin")).unwrap() == *data);
    assert!(diagnostics(&m, id).await.contains("cannot be validated"), "identity uncertain: not continued");
}

#[tokio::test]
async fn an_inconsistent_segment_map_is_never_resumed() {
    let data = random_data(6 * MB, 99);
    let srv = serve(data.clone()).await;
    let e = env("map");
    let id = paused_part_way(&e, &srv, "/map.bin").await;
    let store = SqliteStore::open(&e.db).unwrap();
    let mut rec = store.load(id).unwrap().unwrap();
    rec.segments[1].start += 1; // a gap
    store.save(&rec).unwrap();
    drop(store);
    let m = manager(&e).await;
    m.resume_download(id).await.unwrap();
    wait_status(&m, id, DownloadStatus::Completed).await;
    assert!(std::fs::read(e.root.join("map.bin")).unwrap() == *data);
    assert!(diagnostics(&m, id).await.contains("inconsistent"));
}

/// A server that gives no version at all (or only a date it keeps) changes the file mid-download, same size: the
/// overlap check at the next join notices it before any byte of the new version is written.
async fn changed_without_a_version(path: &str, name: &str, seed: u64) {
    let (old, new) = (random_data(6 * MB, seed), random_data(6 * MB, seed + 1000));
    let srv = serve(old.clone()).await;
    let e = env(name);
    let m = manager(&e).await;
    *srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(15);
    let d = m.create_download(NewDownload { url: srv.url(path), ..Default::default() }).await.unwrap();
    while m.get_download(d.id).await.unwrap().downloaded < MB as u64 {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    srv.set_data(new.clone());
    let failed = wait_status(&m, d.id, DownloadStatus::Failed).await;
    assert_eq!(failed.error_code.as_deref(), Some("source_changed"), "{:?}", failed.error);
    let file = e.root.join(path.rsplit('/').next().unwrap());
    assert!(!file.exists(), "never finished from mixed bytes");
    *srv.state.chunk_delay.lock().unwrap() = Duration::ZERO;
    m.retry_download(d.id).await.unwrap();
    wait_status(&m, d.id, DownloadStatus::Completed).await;
    assert!(std::fs::read(&file).unwrap() == *new, "entirely the new version");
}

#[tokio::test]
async fn a_change_without_any_version_tag_is_caught_at_the_next_join() {
    changed_without_a_version("/noetag/plain.bin", "noversion", 101).await;
}

#[tokio::test]
async fn a_change_with_an_unchanged_date_is_caught_at_the_next_join() {
    changed_without_a_version("/noetag/dated/dated.bin", "samedate", 102).await;
}

#[tokio::test]
async fn a_file_changed_while_paused_with_the_same_date_is_caught_and_never_mixed() {
    // Only a date proves the version, and the server keeps it: the resume check passes, the joins do not.
    let (old, new) = (random_data(6 * MB, 103), random_data(6 * MB, 104));
    let srv = serve(old.clone()).await;
    let e = env("pausedchange");
    let id = paused_part_way(&e, &srv, "/noetag/dated/paused.bin").await;
    srv.set_data(new.clone());
    let m = manager(&e).await;
    m.resume_download(id).await.unwrap();
    let failed = wait_status(&m, id, DownloadStatus::Failed).await;
    assert_eq!(failed.error_code.as_deref(), Some("source_changed"), "{:?}", failed.error);
    assert!(!e.root.join("paused.bin").exists(), "never finished from mixed bytes");
    m.retry_download(id).await.unwrap();
    wait_status(&m, id, DownloadStatus::Completed).await;
    assert!(std::fs::read(e.root.join("paused.bin")).unwrap() == *new, "entirely the new version");
}

/// Segment hashes are built checkpoint by checkpoint while the bytes arrive (and kept for the last checkpoint, which
/// no longer reads the whole file again): each saved one must still be exactly the hash of the server's bytes there.
#[tokio::test]
async fn hashes_built_while_downloading_are_exactly_the_hashes_of_the_bytes() {
    use sha2::{Digest, Sha256};
    let data = random_data(6 * MB, 97);
    let srv = serve(data.clone()).await;
    let e = env("progressive");
    let id = paused_part_way(&e, &srv, "/progressive.bin").await;
    let rec = SqliteStore::open(&e.db).unwrap().load(id).unwrap().unwrap();
    let done: Vec<_> = rec.segments.iter().filter(|s| s.is_complete()).collect();
    assert!(!done.is_empty());
    for s in done {
        let want: String = Sha256::digest(&data[s.start as usize..=s.end as usize]).iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(s.hash.as_deref(), Some(format!("sha256:{want}").as_str()), "segment {} [{}, {}]", s.id, s.start, s.end);
    }
}
