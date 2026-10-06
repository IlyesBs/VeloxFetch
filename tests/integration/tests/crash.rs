//! Phase 13.3: crash testing with **real processes**.
//!
//! A worker process (this same test binary, started with `VELOX_CRASH_ROLE` set) runs a real
//! `DownloadManager` against a test server. It is killed at a chosen instant: either by a
//! failpoint compiled into the crash-test build (`failpoint::hit`, which aborts the process on the
//! spot without unwinding, flushing or closing anything) or from outside with `Child::kill`
//! (`TerminateProcess` / `SIGKILL`). Then a fresh manager starts on what is left on disk and must:
//!
//! * find a structurally sound database (`integrity_check`),
//! * never leave a download in a "running" state that nothing is running,
//! * resume and finish with a file that is **byte for byte** the original (the server sends random
//!   bytes, so a single wrongly trusted byte would show),
//! * leave no `.part` file and no stray file, and exactly one record.
//!
//! Kill points: during the transfer, right after a disk write, between the fsync of the data and
//! the commit of its counters, inside a SQLite transaction (before and after the commit), while
//! pausing, while resuming, and while finishing (before and after the final rename).
//!
//! The process dies of an abrupt stop, not of a power cut: what the operating system had already
//! accepted for the disk survives, as after any process crash. A machine losing power can lose
//! more; that is covered by the ordering rule in `engine::checkpoint` (data fsynced before counters)
//! and by SQLite's own journal, not simulated here.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use download_manager::{
    BatchRequest, DownloadDto, DownloadManager, ListFilter, ManagerConfig, MediaJobDto, MediaRequest, MediaStatus, NewDownload,
    SetQueueConfig,
};
use downloader_core::{DownloadStatus, RetryPolicy};
use persistence::SqliteStore;
use velox_integration_tests::{random_data, serve, Server};

const MB: usize = 1024 * 1024;
const FILE: &str = "crash.bin";

fn base_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("velox-crash-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

fn config(base: &Path) -> ManagerConfig {
    let mut c = ManagerConfig::new(base.join("state").join("velox.db"), base.join("downloads"));
    c.max_concurrent_downloads = 2;
    // Stream downloads: the stand-in for ffmpeg, and working folders of dead processes are cleared at once
    // (the parent only starts a manager after its worker has died).
    c.ffmpeg_path = Some(PathBuf::from(env!("CARGO_BIN_EXE_fake-ffmpeg")));
    c.media_scratch_max_age = Duration::ZERO;
    c.progress_interval = Duration::from_millis(40);
    c.download.min_segment_size = 128 * 1024;
    c.download.checkpoint_interval = Duration::from_millis(50);
    c.download.retry = RetryPolicy {
        max_retries: 3,
        max_throttle_retries: 3,
        base_delay: Duration::from_millis(5),
        max_delay: Duration::from_millis(20),
        max_retry_after: Duration::from_millis(50),
    };
    c
}

// ============================================================================= the worker side

/// Not a test: the entry point of the worker process. Does nothing unless `VELOX_CRASH_ROLE` is set.
#[test]
fn crash_worker_entry() {
    let Ok(role) = std::env::var("VELOX_CRASH_ROLE") else { return };
    const { assert!(failpoint::ENABLED, "the crash tests need the `failpoint/enabled` feature") };
    let url = std::env::var("VELOX_CRASH_URL").unwrap();
    let base = PathBuf::from(std::env::var("VELOX_CRASH_BASE").unwrap());
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(4).enable_all().build().unwrap();
    rt.block_on(async move {
        let m = DownloadManager::new(config(&base)).await.unwrap();
        match role.as_str() {
            "batch" => {
                // Eight links, added (not started) one after the other; the failpoint kills in the middle.
                let links = (1..=8).map(|i| format!("{url}/batch-{i}.bin")).collect::<Vec<_>>().join("\n");
                let r = m.add_batch(BatchRequest { links: Some(links), start: Some(false), ..Default::default() }).await.unwrap();
                eprintln!("batch finished: {r:?}");
                return;
            }
            "settings" => {
                m.set_queue_config(SetQueueConfig { max_active: Some(5), max_connections: None }).await.unwrap();
                tokio::time::sleep(Duration::from_secs(20)).await;
                return;
            }
            "delete" => {
                let d = m.create_download(NewDownload { url, filename: Some(FILE.into()), ..Default::default() }).await.unwrap();
                let t = Instant::now();
                while m.get_download(d.id).await.unwrap().status != DownloadStatus::Completed {
                    assert!(t.elapsed() < Duration::from_secs(30), "never completed");
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                m.delete_download(d.id, true).await.unwrap();
                tokio::time::sleep(Duration::from_secs(20)).await;
                return;
            }
            "media" => {
                let job = m
                    .start_media(MediaRequest { url, filename: Some("clip".into()), combine: Some(true), ..Default::default() })
                    .await
                    .unwrap();
                let t = Instant::now();
                loop {
                    let j = m.list_media().into_iter().find(|j| j.id == job.id).unwrap();
                    match j.status {
                        MediaStatus::Completed => return,
                        s if !s.is_active() => panic!("stream download ended as {s:?}: {:?}", j.error),
                        _ => {}
                    }
                    assert!(t.elapsed() < Duration::from_secs(60), "the stream download timed out");
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            }
            _ => {}
        }
        let existing = m.list_downloads(ListFilter::default()).await.unwrap();
        let id = match existing.first() {
            Some(d) => {
                match d.status {
                    DownloadStatus::Paused => drop(m.resume_download(d.id).await.unwrap()),
                    DownloadStatus::Queued => drop(m.start_download(d.id).await.unwrap()),
                    _ => {}
                }
                d.id
            }
            None => m.create_download(NewDownload { url, filename: Some(FILE.into()), ..Default::default() }).await.unwrap().id,
        };
        if role == "pause" {
            // Let it get going, then ask for a pause. The failpoint kills the process inside the pause.
            let t = Instant::now();
            while m.get_download(id).await.unwrap().downloaded < 1024 * 1024 {
                assert!(t.elapsed() < Duration::from_secs(20), "never got going");
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            let _ = m.pause_download(id).await;
            tokio::time::sleep(Duration::from_secs(30)).await;
            return;
        }
        // role "run": run until the download is complete (a failpoint or the parent ends it earlier).
        let t = Instant::now();
        loop {
            let d = m.get_download(id).await.unwrap();
            match d.status {
                DownloadStatus::Completed => return,
                DownloadStatus::Failed => panic!("download failed: {:?}", d.error),
                _ => {}
            }
            assert!(t.elapsed() < Duration::from_secs(25), "worker timed out in {:?}", d.status);
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    });
}

// ============================================================================= the parent side

struct Worker {
    child: std::process::Child,
}

struct Ended {
    success: bool,
    stderr: String,
}

fn spawn_worker(role: &str, url: &str, base: &Path, failpoint: Option<&str>) -> Worker {
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args(["--exact", "crash_worker_entry", "--nocapture", "--test-threads=1"])
        .env("VELOX_CRASH_ROLE", role)
        .env("VELOX_CRASH_URL", url)
        .env("VELOX_CRASH_BASE", base)
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    cmd.env_remove("VELOX_FAILPOINT");
    if let Some(f) = failpoint {
        cmd.env("VELOX_FAILPOINT", f);
    }
    Worker { child: cmd.spawn().expect("cannot start the worker process") }
}

impl Worker {
    /// Wait for the worker to end by itself (a failpoint fired, or it finished).
    async fn ended(mut self, within: Duration) -> Ended {
        tokio::task::spawn_blocking(move || {
            let t = Instant::now();
            loop {
                if let Some(st) = self.child.try_wait().unwrap() {
                    let mut stderr = String::new();
                    if let Some(mut e) = self.child.stderr.take() {
                        let _ = e.read_to_string(&mut stderr);
                    }
                    return Ended { success: st.success(), stderr };
                }
                if t.elapsed() > within {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    panic!("the worker neither finished nor crashed within {within:?}");
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        })
        .await
        .unwrap()
    }

    /// Kill it from outside after `after`, as a task manager or the OOM killer would.
    async fn kill_after(mut self, after: Duration) -> Ended {
        tokio::time::sleep(after).await;
        tokio::task::spawn_blocking(move || {
            let already_done = self.child.try_wait().unwrap().is_some();
            if !already_done {
                let _ = self.child.kill();
            }
            let st = self.child.wait().unwrap();
            let mut stderr = String::new();
            if let Some(mut e) = self.child.stderr.take() {
                let _ = e.read_to_string(&mut stderr);
            }
            Ended { success: already_done && st.success(), stderr }
        })
        .await
        .unwrap()
    }
}

async fn crash_at(role: &str, url: &str, base: &Path, failpoint: &str) {
    let name = failpoint.split('@').next().unwrap();
    let end = spawn_worker(role, url, base, Some(failpoint)).ended(Duration::from_secs(90)).await;
    assert!(!end.success, "the worker was supposed to crash at {failpoint} but finished normally");
    assert!(end.stderr.contains(&format!("VELOX_FAILPOINT_HIT {name}")), "the worker died, but not at {failpoint}:\n{}", end.stderr);
}

fn db_path(base: &Path) -> PathBuf {
    base.join("state").join("velox.db")
}

/// What a killed process may leave behind must always be a sound database.
fn assert_database_sound(base: &Path) {
    let db = db_path(base);
    if !db.exists() {
        return; // died before the file was even created
    }
    let store = SqliteStore::open(&db).expect("the database cannot be opened after the crash");
    store.integrity_check().expect("the database is damaged after the crash");
}

async fn wait_terminal(m: &DownloadManager, id: i64) -> DownloadDto {
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

/// Start "the app again" on what is left, and check everything the module documentation promises.
/// Returns the state the recovery left the record in (`None` when the crash came before it existed).
async fn recover_and_finish(base: &Path, srv: &Server, url: &str, data: &[u8]) -> Option<DownloadStatus> {
    assert_database_sound(base);
    let m = DownloadManager::new(config(base)).await.expect("the app cannot start after the crash");
    let listed = m.list_downloads(ListFilter::default()).await.unwrap();
    assert!(listed.len() <= 1, "more than one record after a crash: {listed:?}");
    let root = base.join("downloads");
    let after_recovery = listed.first().map(|d| d.status);
    eprintln!("recovered: {:?}", listed.first().map(|d| (d.status, d.downloaded, d.total_size)));
    match listed.first() {
        Some(d) => {
            assert!(
                matches!(d.status, DownloadStatus::Paused | DownloadStatus::Completed | DownloadStatus::Queued | DownloadStatus::Failed),
                "a download is shown as {:?} but no process is running it",
                d.status
            );
            if let Some(t) = d.total_size {
                assert!(d.downloaded <= t, "the record claims {} of {t} bytes", d.downloaded);
            }
            match d.status {
                DownloadStatus::Completed => {
                    assert!(!root.join(format!("{FILE}.part")).exists(), "a completed download still has its .part file");
                }
                DownloadStatus::Failed => drop(m.retry_download(d.id).await.unwrap()), // the user presses Retry
                DownloadStatus::Queued => drop(m.start_download(d.id).await.unwrap()), // started, then killed while waiting for a slot
                _ => drop(m.resume_download(d.id).await.unwrap()),
            }
            let done = wait_terminal(&m, d.id).await;
            assert_eq!(done.status, DownloadStatus::Completed, "{:?}", done.error);
        }
        None => {
            // The crash came before anything was committed: the app must simply work.
            let d =
                m.create_download(NewDownload { url: url.to_string(), filename: Some(FILE.into()), ..Default::default() }).await.unwrap();
            let done = wait_terminal(&m, d.id).await;
            assert_eq!(done.status, DownloadStatus::Completed, "{:?}", done.error);
        }
    }
    let got = std::fs::read(root.join(FILE)).expect("the finished file is missing");
    assert_eq!(got.len(), data.len(), "wrong size after recovery");
    assert!(got == data, "the file differs from the original after recovery (silent corruption)");
    let mut names: Vec<String> = std::fs::read_dir(&root).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().to_string()).collect();
    names.sort();
    assert_eq!(names, [FILE], "stray files after recovery: {names:?}");
    assert_eq!(m.list_downloads(ListFilter::default()).await.unwrap().len(), 1);
    m.shutdown().await;
    let _ = srv; // kept alive until here
    after_recovery
}

async fn slow_server(name: &str, size: usize, delay_ms: u64) -> (Server, Arc<Vec<u8>>, PathBuf) {
    let data = random_data(size, name.len() as u64 * 7919 + size as u64);
    let srv = serve(data.clone()).await;
    *srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(delay_ms);
    (srv, data, base_dir(name))
}

// ============================================================================= scenarios

/// (test name, failpoint, what happens there)
async fn scenario(name: &str, failpoint: &str) -> Option<DownloadStatus> {
    let (srv, data, base) = slow_server(name, 6 * MB, 12).await;
    let url = srv.url(&format!("/{FILE}"));
    crash_at("run", &url, &base, failpoint).await;
    *srv.state.chunk_delay.lock().unwrap() = Duration::ZERO;
    // The server may still be writing to the dead worker's connections (the kernel accepts a few
    // writes before it learns the peer is gone): let those end, so they are not counted as recovery.
    let t = Instant::now();
    while srv.state.active.load(std::sync::atomic::Ordering::SeqCst) > 0 && t.elapsed() < Duration::from_secs(5) {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let served_before = srv.state.bytes_served.load(std::sync::atomic::Ordering::SeqCst);
    let st = recover_and_finish(&base, &srv, &url, &data).await;
    let served_after = srv.state.bytes_served.load(std::sync::atomic::Ordering::SeqCst);
    // Whatever was durable is not fetched again: the whole recovery costs less than a full second copy.
    assert!(
        served_after - served_before <= data.len() as u64 + 64 * 1024,
        "recovery re-downloaded more than the file: {} bytes",
        served_after - served_before
    );
    st
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn killed_right_after_a_disk_write_early_in_the_transfer() {
    scenario("kill-write-early", "disk.after_write@4").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn killed_right_after_a_disk_write_late_in_the_transfer() {
    scenario("kill-write-late", "disk.after_write@60").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn killed_between_the_fsync_of_the_data_and_the_commit_of_its_counters() {
    scenario("kill-fsync", "checkpoint.after_fsync@3").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn killed_inside_the_very_first_sqlite_transaction() {
    // Nothing was ever committed: no record, and the app must simply work afterwards.
    let st = scenario("kill-tx-first", "store.before_commit@1").await;
    assert_eq!(st, None, "an uncommitted record must not exist after the crash");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn killed_inside_a_sqlite_transaction_while_downloading() {
    scenario("kill-tx-mid", "store.before_commit@6").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn killed_just_after_a_sqlite_commit() {
    scenario("kill-tx-after", "store.after_commit@5").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn killed_while_pausing() {
    let (srv, data, base) = slow_server("kill-pause", 6 * MB, 12).await;
    let url = srv.url(&format!("/{FILE}"));
    crash_at("pause", &url, &base, "pause.before_persist").await;
    *srv.state.chunk_delay.lock().unwrap() = Duration::ZERO;
    let st = recover_and_finish(&base, &srv, &url, &data).await;
    assert_eq!(st, Some(DownloadStatus::Paused), "a download killed while pausing comes back paused, with its progress");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn killed_while_resuming_and_again_while_resumed_writes() {
    let (srv, data, base) = slow_server("kill-resume", 6 * MB, 12).await;
    let url = srv.url(&format!("/{FILE}"));
    crash_at("run", &url, &base, "disk.after_write@30").await; // first crash: mid-transfer
    assert_database_sound(&base);
    crash_at("run", &url, &base, "resume.after_load").await; // second crash: while resuming
    assert_database_sound(&base);
    crash_at("run", &url, &base, "disk.after_write@8").await; // third: in the resumed transfer
    *srv.state.chunk_delay.lock().unwrap() = Duration::ZERO;
    let st = recover_and_finish(&base, &srv, &url, &data).await;
    assert_eq!(st, Some(DownloadStatus::Paused));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn killed_before_the_final_rename_the_finished_data_is_not_downloaded_again() {
    let (srv, data, base) = slow_server("kill-final-before", 3 * MB, 2).await;
    let url = srv.url(&format!("/{FILE}"));
    crash_at("run", &url, &base, "finalize.before_rename").await;
    let served = srv.state.bytes_served.load(std::sync::atomic::Ordering::SeqCst);
    assert!(served >= data.len() as u64, "the download had not finished before the kill");
    let st = recover_and_finish(&base, &srv, &url, &data).await;
    assert_eq!(st, Some(DownloadStatus::Paused));
    let extra = srv.state.bytes_served.load(std::sync::atomic::Ordering::SeqCst) - served;
    assert!(extra < 64 * 1024, "the complete data was fetched again ({extra} bytes)");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn killed_right_after_the_final_rename_the_file_is_recognised_as_finished() {
    let (srv, data, base) = slow_server("kill-final-after", 3 * MB, 2).await;
    let url = srv.url(&format!("/{FILE}"));
    crash_at("run", &url, &base, "finalize.after_rename").await;
    let served = srv.state.bytes_served.load(std::sync::atomic::Ordering::SeqCst);
    let st = recover_and_finish(&base, &srv, &url, &data).await;
    assert_eq!(st, Some(DownloadStatus::Completed), "the file was already in place: recovery must say so, not download it again");
    let extra = srv.state.bytes_served.load(std::sync::atomic::Ordering::SeqCst) - served;
    assert_eq!(extra, 0, "recovery contacted the server for a finished file");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_one_connection_download_killed_mid_transfer_restarts_cleanly() {
    let (srv, data, base) = slow_server("kill-sequential", 3 * MB, 6).await;
    let url = srv.url(&format!("/norange/{FILE}"));
    crash_at("run", &url, &base, "disk.after_write@10").await;
    *srv.state.chunk_delay.lock().unwrap() = Duration::ZERO;
    recover_and_finish(&base, &srv, &url, &data).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn killed_from_outside_at_random_moments_again_and_again() {
    // TerminateProcess / SIGKILL at unpredictable instants (a fixed seed, so a failure can be replayed),
    // then the app is started again on the wreckage, over and over, until the download completes.
    let (srv, data, base) = slow_server("kill-random", 48 * MB, 40).await;
    let url = srv.url(&format!("/{FILE}"));
    let mut x: u64 = 0x2545F4914F6CDD1D;
    let mut delays = Vec::new();
    for _ in 0..8 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        delays.push(600 + (x % 1200)); // 0.6 - 1.8 s: past start-up, well before the end
    }
    for (i, ms) in delays.iter().enumerate() {
        let end = spawn_worker("run", &url, &base, None).kill_after(Duration::from_millis(*ms)).await;
        if end.success {
            break; // finished before the kill: nothing left to test
        }
        assert_database_sound(&base);
        eprintln!("kill #{} after {ms} ms", i + 1);
    }
    *srv.state.chunk_delay.lock().unwrap() = Duration::ZERO;
    recover_and_finish(&base, &srv, &url, &data).await;
    // The server was never asked for many times the file over eight restarts.
    let total = srv.state.bytes_served.load(std::sync::atomic::Ordering::SeqCst);
    assert!(total < data.len() as u64 * 4, "{total} bytes served for a {} byte file over 8 kills", data.len());
}

// ============================================================================= power loss and torn state

/// What a power cut can leave that a process kill cannot, applied to the wreckage of a killed worker:
///
/// * the tail of the SQLite write-ahead log is lost (`wal_kept` = the fraction that survived, 0 = all of it),
/// * the bytes of the `.part` file that the database does not claim are lost (garbage or zeros: the operating
///   system had not written them yet), and, with `shorten_part`, the file itself came back shorter.
///
/// Before the loss it also checks the promise the engine makes: **every byte the database claims is really in
/// the file and equals the original** (the data is fsynced before the counters are committed).
fn simulate_power_loss(base: &Path, data: &[u8], wal_kept: f64, shorten_part: bool) {
    let db = db_path(base);
    let wal = PathBuf::from(format!("{}-wal", db.display()));
    let shm = PathBuf::from(format!("{}-shm", db.display()));
    if let Ok(len) = std::fs::metadata(&wal).map(|m| m.len()) {
        let keep = (len as f64 * wal_kept) as u64;
        if keep == 0 {
            let _ = std::fs::remove_file(&wal);
        } else {
            let f = std::fs::OpenOptions::new().write(true).open(&wal).unwrap();
            f.set_len(keep).unwrap();
        }
    }
    let _ = std::fs::remove_file(&shm);
    let store = SqliteStore::open(&db).expect("the database cannot be opened after the power cut");
    store.integrity_check().expect("the database is damaged after the power cut");
    let records = store.list().unwrap();
    for rec in records {
        let part = downloader_core::part_path(&rec.destination);
        let Ok(mut bytes) = std::fs::read(&part) else { continue };
        let mut claimed = vec![false; bytes.len()];
        for seg in &rec.segments {
            let (a, b) = (seg.start as usize, (seg.start + seg.downloaded) as usize);
            if b > bytes.len() {
                continue; // the file is shorter than the claim: recovery must notice (checked below)
            }
            assert!(
                bytes[a..b] == data[a..b],
                "the database claims {} bytes of segment {} that are not in the file",
                seg.downloaded,
                seg.id
            );
            claimed[a..b].iter_mut().for_each(|c| *c = true);
        }
        // The rest was never flushed: garbage.
        for (i, b) in bytes.iter_mut().enumerate() {
            if !claimed[i] {
                *b = 0xAB;
            }
        }
        if shorten_part {
            bytes.truncate(bytes.len() / 2);
        }
        std::fs::write(&part, &bytes).unwrap();
    }
    drop(store);
}

async fn power_loss(name: &str, failpoint: &str, wal_kept: f64, shorten_part: bool) {
    let (srv, data, base) = slow_server(name, 6 * MB, 12).await;
    let url = srv.url(&format!("/{FILE}"));
    crash_at("run", &url, &base, failpoint).await;
    *srv.state.chunk_delay.lock().unwrap() = Duration::ZERO;
    simulate_power_loss(&base, &data, wal_kept, shorten_part);
    recover_and_finish(&base, &srv, &url, &data).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_power_cut_that_loses_every_unflushed_byte_loses_no_claimed_byte() {
    power_loss("power-plain", "disk.after_write@45", 1.0, false).await;
    power_loss("power-fsync", "checkpoint.after_fsync@4", 1.0, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_power_cut_that_also_loses_the_end_of_the_database_log_only_loses_progress() {
    for (i, kept) in [0.75, 0.4, 0.05, 0.0].into_iter().enumerate() {
        power_loss(&format!("power-wal-{i}"), "disk.after_write@50", kept, false).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_power_cut_that_leaves_a_shorter_partial_file_restarts_that_download_cleanly() {
    power_loss("power-short", "disk.after_write@45", 1.0, true).await;
}

// ============================================================================= more kill points

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn killed_just_after_the_partial_file_was_reserved() {
    // The record exists (the manager saves it first), the .part file is empty of data: it must simply resume.
    let st = scenario("kill-prealloc", "disk.after_preallocate").await;
    assert!(st.is_some(), "the record is written before the file is reserved");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn killed_while_the_history_line_of_a_finished_download_is_written() {
    for (i, fp) in ["history.before", "history.after"].into_iter().enumerate() {
        let (srv, data, base) = slow_server(&format!("kill-history-{i}"), 2 * MB, 2).await;
        let url = srv.url(&format!("/{FILE}"));
        crash_at("run", &url, &base, fp).await;
        let served = srv.state.bytes_served.load(std::sync::atomic::Ordering::SeqCst);
        let st = recover_and_finish(&base, &srv, &url, &data).await;
        assert_eq!(st, Some(DownloadStatus::Completed), "{fp}: the file was already complete");
        assert_eq!(srv.state.bytes_served.load(std::sync::atomic::Ordering::SeqCst), served, "{fp}: a finished file was fetched again");
        let m = DownloadManager::new(config(&base)).await.unwrap();
        let history = m.list_history(50).await.unwrap();
        assert!(history.len() <= 1, "{fp}: duplicated history: {history:?}");
        m.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn killed_while_deleting_a_download_it_can_be_deleted_again() {
    for (i, fp) in ["delete.after_files", "delete.before"].into_iter().enumerate() {
        let (srv, _data, base) = slow_server(&format!("kill-delete-{i}"), MB, 0).await;
        let url = srv.url(&format!("/{FILE}"));
        crash_at("delete", &url, &base, fp).await;
        assert_database_sound(&base);
        let m = DownloadManager::new(config(&base)).await.expect("the app cannot start after a kill during a delete");
        let left = m.list_downloads(ListFilter::default()).await.unwrap();
        assert!(left.len() <= 1, "{fp}: {left:?}");
        for d in left {
            // The record is still there (the file may be gone): removing it must work, and end in a clean folder.
            m.delete_download(d.id, true).await.unwrap_or_else(|e| panic!("{fp}: cannot delete after the crash: {e}"));
        }
        assert!(m.list_downloads(ListFilter::default()).await.unwrap().is_empty(), "{fp}");
        let root = base.join("downloads");
        let names: Vec<_> = std::fs::read_dir(&root).unwrap().flatten().map(|e| e.file_name()).collect();
        assert!(names.is_empty(), "{fp}: files left in the download folder: {names:?}");
        m.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn killed_in_the_middle_of_adding_a_batch_nothing_is_duplicated_or_lost_when_it_is_added_again() {
    let srv = serve(random_data(MB, 77)).await;
    let base = base_dir("kill-batch");
    let url = format!("http://{}", srv.addr);
    crash_at("batch", &url, &base, "batch.after_item@4").await;
    assert_database_sound(&base);
    let m = DownloadManager::new(config(&base)).await.expect("the app cannot start after a kill during a batch");
    let partial = m.list_downloads(ListFilter::default()).await.unwrap();
    assert_eq!(partial.len(), 4, "exactly the links added before the kill exist: {partial:?}");
    assert!(partial.iter().all(|d| d.status == DownloadStatus::Queued), "nothing was started");
    // The user adds the same list again: the four known links are recognised, the other four are added.
    let links = (1..=8).map(|i| format!("{url}/batch-{i}.bin")).collect::<Vec<_>>().join("\n");
    let r = m.add_batch(BatchRequest { links: Some(links), start: Some(false), ..Default::default() }).await.unwrap();
    assert_eq!((r.added, r.existing, r.failed), (4, 4, 0), "{r:?}");
    assert_eq!(m.list_downloads(ListFilter::default()).await.unwrap().len(), 8);
    m.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn killed_while_a_setting_is_written_it_is_either_the_old_value_or_the_new_one() {
    for (fp, expected) in [("settings.before", 2usize), ("settings.after", 5usize)] {
        let srv = serve(random_data(1024, 1)).await;
        let base = base_dir(&format!("kill-{fp}"));
        crash_at("settings", &srv.url("/x"), &base, fp).await;
        assert_database_sound(&base);
        let m = DownloadManager::new(config(&base)).await.expect("the app cannot start");
        let q = m.get_queue_state().await.unwrap();
        assert_eq!(q.max_active, expected, "{fp}");
        m.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn half_written_or_garbage_settings_fall_back_to_defaults_and_never_stop_the_app() {
    let srv = serve(random_data(1024, 2)).await;
    let base = base_dir("torn-settings");
    {
        // A first start creates the database; then damage its settings the way a torn write or a bad edit would.
        let m = DownloadManager::new(config(&base)).await.unwrap();
        m.shutdown().await;
    }
    let store = SqliteStore::open(db_path(&base)).unwrap();
    for (k, v) in [
        ("queue.max_active", "99999999999999999999"),
        ("queue.max_connections", "-3"),
        ("queue.paused", "\u{0}\u{0}"),
        ("queue.schedule", "{\"enabled\":tru"),
        ("queue.held", "1,,x,9223372036854775808,-"),
        ("speed_limit.global", "fast"),
        ("browser.confirm_downloads", ""),
        ("clipboard.watch", "\u{1f4a5}"),
    ] {
        store.set_setting(k, v).unwrap();
    }
    drop(store);
    let m = DownloadManager::new(config(&base)).await.expect("garbage settings must not stop the app from starting");
    let q = m.get_queue_state().await.unwrap();
    assert!((1..=32).contains(&q.max_active) && (1..=64).contains(&q.max_connections), "{q:?}");
    assert!(!q.schedule.enabled, "a broken schedule must not silently hold downloads");
    // And it still downloads.
    let d = m.create_download(NewDownload { url: srv.url("/ok.bin"), ..Default::default() }).await.unwrap();
    assert_eq!(wait_terminal(&m, d.id).await.status, DownloadStatus::Completed);
    m.shutdown().await;
}

// ============================================================================= stream (HLS) downloads

struct Stream {
    video: Vec<u8>,
    audio: Vec<u8>,
}

/// A master playlist with one video quality and a separate audio track, so the download has to be combined.
fn hls_on(srv: &Server) -> Stream {
    srv.put(
        "/hls/master.m3u8",
        "#EXTM3U\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",NAME=\"English\",LANGUAGE=\"en\",DEFAULT=YES,URI=\"audio.m3u8\"\n\
#EXT-X-STREAM-INF:BANDWIDTH=2800000,RESOLUTION=1280x720,AUDIO=\"aud\"\nvideo.m3u8\n",
    );
    let playlist = |dir: &str, names: &[String]| {
        let mut s = String::from("#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:6\n#EXT-X-PLAYLIST-TYPE:VOD\n");
        for n in names {
            s.push_str(&format!("#EXTINF:6.0,\n{dir}/{n}\n"));
        }
        s.push_str("#EXT-X-ENDLIST\n");
        s
    };
    let vnames: Vec<String> = (0..14).map(|i| format!("v{i}.ts")).collect();
    let anames: Vec<String> = (0..6).map(|i| format!("a{i}.aac")).collect();
    srv.put("/hls/video.m3u8", playlist("video", &vnames));
    srv.put("/hls/audio.m3u8", playlist("audio", &anames));
    let (mut video, mut audio) = (Vec::new(), Vec::new());
    for (i, n) in vnames.iter().enumerate() {
        let seg = random_data(120_000 + i * 1000, 500 + i as u64).to_vec();
        srv.put(&format!("/hls/video/{n}"), seg.clone());
        video.extend(seg);
    }
    for (i, n) in anames.iter().enumerate() {
        let seg = random_data(20_000, 900 + i as u64).to_vec();
        srv.put(&format!("/hls/audio/{n}"), seg.clone());
        audio.extend(seg);
    }
    Stream { video, audio }
}

/// Every name in the folder, with hidden working folders included.
fn listing(root: &Path) -> Vec<String> {
    let mut v: Vec<String> =
        std::fs::read_dir(root).map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect()).unwrap_or_default();
    v.sort();
    v
}

/// After a killed stream download: the app starts, sweeps the dead working folder, and whatever is in the
/// folder is either nothing or the complete, correct file. Then the same download works.
async fn recover_stream(base: &Path, srv: &Server, master: &str, want: &[u8]) {
    let root = base.join("downloads");
    let m = DownloadManager::new(config(base)).await.expect("the app cannot start after a kill during a stream download");
    let names = listing(&root);
    assert!(!names.iter().any(|n| n.starts_with(".velox-media-")), "a dead working folder was left behind: {names:?}");
    for n in &names {
        let meta = std::fs::metadata(root.join(n)).unwrap();
        assert!(meta.len() > 0, "an empty placeholder file was left behind: {n}");
        assert_eq!(std::fs::read(root.join(n)).unwrap(), want, "{n} is not the complete file");
    }
    assert!(m.list_media().is_empty(), "stream jobs are not kept across a restart");
    // Run it again, as the user would.
    let job = m
        .start_media(MediaRequest { url: srv.url(master), filename: Some("again".into()), combine: Some(true), ..Default::default() })
        .await
        .unwrap();
    let t = Instant::now();
    let done: MediaJobDto = loop {
        let j = m.list_media().into_iter().find(|j| j.id == job.id).unwrap();
        if !j.status.is_active() {
            break j;
        }
        assert!(t.elapsed() < Duration::from_secs(30), "the second run got stuck: {j:?}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    };
    assert_eq!(done.status, MediaStatus::Completed, "{done:?}");
    assert_eq!(std::fs::read(root.join("again.mp4")).unwrap(), want);
    assert!(!listing(&root).iter().any(|n| n.starts_with(".velox-media-")), "the working folder was not removed");
    m.shutdown().await;
}

async fn stream_crash(name: &str, failpoint: &str) {
    let srv = serve(random_data(16, 3)).await;
    let stream = hls_on(&srv);
    *srv.state.route_delay.lock().unwrap() = Duration::from_millis(40);
    let base = base_dir(name);
    let master = "/hls/master.m3u8";
    crash_at("media", &srv.url(master), &base, failpoint).await;
    *srv.state.route_delay.lock().unwrap() = Duration::ZERO;
    let want = [b"FAKEMUX\n".to_vec(), stream.video, stream.audio].concat();
    recover_stream(&base, &srv, master, &want).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stream_download_killed_in_the_middle_of_its_segments_leaves_nothing_behind() {
    stream_crash("kill-media-seg", "media.after_segment@9").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stream_download_killed_just_before_and_just_after_combining_leaves_nothing_behind() {
    stream_crash("kill-media-before-mux", "media.before_mux").await;
    stream_crash("kill-media-after-mux", "media.after_mux").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stream_download_killed_while_its_file_is_put_in_place_is_complete_or_absent_never_empty() {
    stream_crash("kill-media-link", "media.after_link").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stream_download_killed_from_outside_at_random_moments_never_corrupts_the_folder() {
    let srv = serve(random_data(16, 4)).await;
    let stream = hls_on(&srv);
    *srv.state.route_delay.lock().unwrap() = Duration::from_millis(60);
    let base = base_dir("kill-media-random");
    let master = "/hls/master.m3u8";
    let mut x: u64 = 0x1234_5678_9ABC_DEF1;
    for _ in 0..5 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let ms = 400 + x % 1400;
        let end = spawn_worker("media", &srv.url(master), &base, None).kill_after(Duration::from_millis(ms)).await;
        if end.success {
            break;
        }
        assert_database_sound(&base);
    }
    *srv.state.route_delay.lock().unwrap() = Duration::ZERO;
    let want = [b"FAKEMUX\n".to_vec(), stream.video, stream.audio].concat();
    recover_stream(&base, &srv, master, &want).await;
}

// ============================================================================= disk and database faults

/// The worker meets a failing disk (an injected error from the N-th call on, as a full disk stays full);
/// then the fault is gone (a fresh process without it) and the user retries.
async fn disk_fault(name: &str, failpoint: &str, must_fail: bool) -> (Server, Arc<Vec<u8>>, PathBuf, String) {
    let (srv, data, base) = slow_server(name, 6 * MB, 12).await;
    let url = srv.url(&format!("/{FILE}"));
    let t = Instant::now();
    let end = spawn_worker("run", &url, &base, Some(failpoint)).ended(Duration::from_secs(90)).await;
    let point = failpoint.split(['@', ':']).next().unwrap();
    assert!(end.stderr.contains(&format!("VELOX_FAILPOINT_FAIL {point}")), "the fault was never injected:\n{}", end.stderr);
    if must_fail {
        assert!(!end.success, "{failpoint}: the download should have failed");
        assert!(end.stderr.contains("download failed"), "{failpoint}: it did not fail cleanly:\n{}", end.stderr);
        assert!(
            t.elapsed() < Duration::from_secs(30),
            "{failpoint}: a failing disk must end the download quickly, it took {:?}",
            t.elapsed()
        );
    }
    *srv.state.chunk_delay.lock().unwrap() = Duration::ZERO;
    (srv, data, base, url)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_disk_that_fills_up_mid_transfer_fails_clearly_and_the_retry_finishes_the_file() {
    let (srv, data, base, url) = disk_fault("fault-full", "disk.write@30:enospc", true).await;
    let st = recover_and_finish(&base, &srv, &url, &data).await;
    assert_eq!(st, Some(DownloadStatus::Failed), "the record says why it stopped");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_disk_that_is_already_full_when_the_file_is_reserved_fails_clearly_and_the_retry_works() {
    let (srv, data, base, url) = disk_fault("fault-full-at-start", "disk.preallocate@1:enospc", true).await;
    recover_and_finish(&base, &srv, &url, &data).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_file_that_cannot_be_renamed_because_it_is_locked_keeps_its_data_and_the_retry_needs_no_download() {
    let (srv, data, base, url) = disk_fault("fault-locked", "finalize.rename@1:denied", true).await;
    let served = srv.state.bytes_served.load(std::sync::atomic::Ordering::SeqCst);
    let st = recover_and_finish(&base, &srv, &url, &data).await;
    assert_eq!(st, Some(DownloadStatus::Failed));
    let extra = srv.state.bytes_served.load(std::sync::atomic::Ordering::SeqCst) - served;
    assert!(extra < 64 * 1024, "the complete data was downloaded again after a locked-file error ({extra} bytes)");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_disk_that_disappears_mid_transfer_fails_clearly_and_the_retry_finishes_the_file() {
    let (srv, data, base, url) = disk_fault("fault-gone", "disk.write@25:notfound", true).await;
    recover_and_finish(&base, &srv, &url, &data).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_disk_that_cannot_flush_never_lets_the_database_claim_unsaved_bytes() {
    // Every fsync fails: the checkpoints cannot commit anything, but the transfer itself is fine and the
    // file is exact. What the database says about progress must never exceed what reached the disk.
    let (srv, data, base, _url) = disk_fault("fault-fsync", "disk.fsync@1:io", false).await;
    let store = SqliteStore::open(db_path(&base)).unwrap();
    for rec in store.list().unwrap() {
        if rec.status != DownloadStatus::Completed {
            let part = std::fs::read(downloader_core::part_path(&rec.destination)).unwrap_or_default();
            for seg in &rec.segments {
                let (a, b) = (seg.start as usize, (seg.start + seg.downloaded) as usize);
                assert!(b <= part.len() && part[a..b] == data[a..b], "the database claims unsaved bytes");
            }
        }
    }
    drop(store);
    recover_and_finish(&base, &srv, &_url, &data).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_database_that_cannot_be_written_never_produces_a_corrupt_file() {
    // The state database refuses writes from the 6th one on (its disk is full). Whatever the download does
    // (stop, fail, or finish), nothing wrong may be published, and after the fault it must be completable.
    let (srv, data, base) = slow_server("fault-db", 6 * MB, 12).await;
    let url = srv.url(&format!("/{FILE}"));
    let end = spawn_worker("run", &url, &base, Some("store.write@6:enospc")).ended(Duration::from_secs(90)).await;
    assert!(end.stderr.contains("VELOX_FAILPOINT_FAIL store.write"), "the fault was never injected:\n{}", end.stderr);
    *srv.state.chunk_delay.lock().unwrap() = Duration::ZERO;
    let root = base.join("downloads");
    if let Ok(published) = std::fs::read(root.join(FILE)) {
        assert!(published == *data, "a file was published that differs from the original");
    }
    recover_and_finish(&base, &srv, &url, &data).await;
}
