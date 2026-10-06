//! Phase 13.1: the download state machine, exhaustively.
//!
//! Every command (start, resume, pause, cancel, retry, delete) is applied to a download in every
//! state (queued, downloading, paused, failed, completed, cancelled). The table below is the
//! specification: what is allowed, what must be refused with `invalid_state`, and that a refused
//! command changes nothing. Then a burst of racing commands must leave one consistent state.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use download_manager::{ApiError, DownloadManager, ManagerConfig, ManagerError, NewDownload};
use downloader_core::{DownloadStatus, RetryPolicy};
use velox_integration_tests::{random_data, serve, Server};

use DownloadStatus::*;

const MB: usize = 1024 * 1024;

struct Env {
    m: DownloadManager,
    root: PathBuf,
    srv: Server,
    data: Arc<Vec<u8>>,
}

async fn env(name: &str) -> Env {
    let base = std::env::temp_dir().join(format!("velox-sm-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let mut c = ManagerConfig::new(base.join("state").join("velox.db"), base.join("downloads"));
    c.progress_interval = Duration::from_millis(40);
    c.download.min_segment_size = 64 * 1024;
    c.download.checkpoint_interval = Duration::from_millis(50);
    c.download.retry = RetryPolicy {
        max_retries: 3, // a loaded machine must not turn a hiccup into a Failed download
        max_throttle_retries: 3,
        base_delay: Duration::from_millis(5),
        max_delay: Duration::from_millis(20),
        max_retry_after: Duration::from_millis(50),
    };
    let data = random_data(6 * MB, 401);
    let srv = serve(data.clone()).await;
    Env { m: DownloadManager::new(c).await.unwrap(), root: base.join("downloads"), srv, data }
}

async fn wait_status(m: &DownloadManager, id: i64, want: DownloadStatus) {
    let t = Instant::now();
    loop {
        let d = m.get_download(id).await.unwrap();
        if d.status == want {
            return;
        }
        assert!(t.elapsed() < Duration::from_secs(20), "download {id} stuck in {:?}, wanted {want:?} ({:?})", d.status, d.error);
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
}

/// Make a download that is in `state`, and return its id.
async fn make(e: &Env, state: DownloadStatus, tag: &str) -> i64 {
    let slow = |on: bool| *e.srv.state.chunk_delay.lock().unwrap() = if on { Duration::from_millis(25) } else { Duration::ZERO };
    let file = |name: &str| format!("/{tag}-{name}.bin");
    match state {
        Queued => {
            let d =
                e.m.create_download(NewDownload { url: e.srv.url(&file("q")), start: Some(false), ..Default::default() }).await.unwrap();
            assert_eq!(d.status, Queued);
            d.id
        }
        Downloading => {
            slow(true);
            let d = e.m.create_download(NewDownload { url: e.srv.url(&file("d")), ..Default::default() }).await.unwrap();
            wait_status(&e.m, d.id, Downloading).await;
            d.id
        }
        Paused => {
            slow(true);
            let d = e.m.create_download(NewDownload { url: e.srv.url(&file("p")), ..Default::default() }).await.unwrap();
            wait_status(&e.m, d.id, Downloading).await;
            e.m.pause_download(d.id).await.unwrap();
            wait_status(&e.m, d.id, Paused).await;
            d.id
        }
        Failed => {
            let d = e.m.create_download(NewDownload { url: e.srv.url("/404/missing.bin"), ..Default::default() }).await.unwrap();
            wait_status(&e.m, d.id, Failed).await;
            d.id
        }
        Completed => {
            slow(false);
            let d = e.m.create_download(NewDownload { url: e.srv.url(&file("c")), ..Default::default() }).await.unwrap();
            wait_status(&e.m, d.id, Completed).await;
            d.id
        }
        Cancelled => {
            slow(true);
            let d = e.m.create_download(NewDownload { url: e.srv.url(&file("x")), ..Default::default() }).await.unwrap();
            wait_status(&e.m, d.id, Downloading).await;
            e.m.cancel_download(d.id).await.unwrap();
            wait_status(&e.m, d.id, Cancelled).await;
            d.id
        }
        Verifying => unreachable!("a moment, not a resting state"),
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Action {
    Start,
    Resume,
    Pause,
    Cancel,
    Retry,
    Delete,
}

async fn apply(m: &DownloadManager, a: Action, id: i64) -> Result<(), ManagerError> {
    match a {
        Action::Start => m.start_download(id).await.map(drop),
        Action::Resume => m.resume_download(id).await.map(drop),
        Action::Pause => m.pause_download(id).await.map(drop),
        Action::Cancel => m.cancel_download(id).await.map(drop),
        Action::Retry => m.retry_download(id).await.map(drop),
        Action::Delete => m.delete_download(id, false).await,
    }
}

/// `Some(status)` = allowed and the resting status afterwards (`None` there = "keeps going": it is
/// running, so it may be queued, downloading, or already done). `Err` = must be refused.
enum Expect {
    Refused,
    Runs,
    Becomes(DownloadStatus),
    Gone,
}

fn expected(state: DownloadStatus, a: Action) -> Expect {
    use Action::*;
    use Expect::*;
    match (a, state) {
        (Delete, _) => Gone,
        (Start, Queued | Downloading | Paused | Failed) => Runs,
        (Start, Completed | Cancelled) => Refused,
        // A `Queued` download here was added without starting ("Download later"): Resume starts it,
        // Pause parks it (so the schedule leaves it alone).
        (Resume, Queued | Downloading | Paused) => Runs,
        (Resume, Failed | Completed | Cancelled) => Refused,
        (Pause, Queued | Downloading | Paused) => Becomes(Paused),
        (Pause, Failed | Completed | Cancelled) => Refused,
        (Cancel, Queued | Downloading | Paused | Failed) => Becomes(Cancelled),
        (Cancel, Completed | Cancelled) => Refused,
        (Retry, Failed) => Runs,
        (Retry, _) => Refused,
        (_, Verifying) => unreachable!(),
    }
}

#[tokio::test]
async fn every_command_in_every_state_does_what_the_table_says_and_refusals_change_nothing() {
    let states = [Queued, Downloading, Paused, Failed, Completed, Cancelled];
    let actions = [Action::Start, Action::Resume, Action::Pause, Action::Cancel, Action::Retry, Action::Delete];
    let e = env("table").await;
    let mut checked = 0;
    for &state in &states {
        for &action in &actions {
            let id = make(&e, state, &format!("{state:?}-{action:?}").to_lowercase()).await;
            let before = e.m.get_download(id).await.unwrap();
            let result = apply(&e.m, action, id).await;
            let what = format!("{action:?} on a {state:?} download");
            match expected(state, action) {
                Expect::Refused => {
                    let err = result.expect_err(&format!("{what} should be refused"));
                    assert!(matches!(err, ManagerError::InvalidState { .. }), "{what}: wrong error {err:?}");
                    assert_eq!(ApiError::from(err).code, "invalid_state", "{what}");
                    let after = e.m.get_download(id).await.unwrap();
                    assert_eq!(after.status, before.status, "{what}: a refused command changed the state");
                    if state == Downloading {
                        // It keeps running on its own: progress may only move forward.
                        assert!(after.downloaded >= before.downloaded, "{what}: progress went backwards");
                    } else {
                        assert_eq!(after.downloaded, before.downloaded, "{what}: a refused command changed the progress");
                    }
                }
                Expect::Gone => {
                    result.unwrap_or_else(|err| panic!("{what} should work: {err:?}"));
                    assert!(matches!(e.m.get_download(id).await, Err(ManagerError::NotFound(_))), "{what}: still listed");
                }
                Expect::Becomes(want) => {
                    result.unwrap_or_else(|err| panic!("{what} should work: {err:?}"));
                    wait_status(&e.m, id, want).await;
                    if want == Cancelled {
                        let files: Vec<String> =
                            std::fs::read_dir(&e.root).unwrap().flatten().map(|f| f.file_name().to_string_lossy().to_string()).collect();
                        assert!(
                            !files
                                .iter()
                                .any(|f| f.starts_with(&format!("{}-", format!("{state:?}-{action:?}").to_lowercase()))
                                    && f.ends_with(".part")),
                            "{what}: a partial file was kept: {files:?}"
                        );
                    }
                }
                Expect::Runs => {
                    result.unwrap_or_else(|err| panic!("{what} should work: {err:?}"));
                    let after = e.m.get_download(id).await.unwrap();
                    assert!(matches!(after.status, Queued | Downloading | Completed | Failed), "{what}: ended up {:?}", after.status);
                }
            }
            checked += 1;
            // Let whatever is running finish or stop, so the next case starts on a quiet server.
            if let Ok(d) = e.m.get_download(id).await {
                if matches!(d.status, Queued | Downloading) {
                    *e.srv.state.chunk_delay.lock().unwrap() = Duration::ZERO;
                    let _ = e.m.cancel_download(id).await;
                }
            }
        }
    }
    assert_eq!(checked, 36, "every state x command pair was covered");
    let _ = &e.data;
}

#[tokio::test]
async fn a_burst_of_racing_commands_leaves_one_consistent_state_and_never_a_corrupt_file() {
    for round in 0..6u32 {
        let e = env(&format!("race-{round}")).await;
        *e.srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(6);
        let d =
            e.m.create_download(NewDownload { url: e.srv.url("/race.bin"), filename: Some("race.bin".into()), ..Default::default() })
                .await
                .unwrap();
        wait_status(&e.m, d.id, Downloading).await;

        // 40 commands fired at the same time, in a different order every round.
        let mut x = 0x9E37_79B9_7F4A_7C15u64 ^ (round as u64 + 1);
        let mut tasks = Vec::new();
        for _ in 0..40 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let action = [Action::Start, Action::Resume, Action::Pause, Action::Retry][(x % 4) as usize];
            let (m, id) = (e.m.clone(), d.id);
            tasks.push(tokio::spawn(async move {
                let r = apply(&m, action, id).await;
                // Only "invalid state" refusals are acceptable; anything else is a bug (panic, internal error, lost download).
                if let Err(err) = r {
                    assert!(matches!(err, ManagerError::InvalidState { .. }), "{action:?}: unexpected error {err:?}");
                }
            }));
        }
        for t in tasks {
            t.await.expect("a command task panicked");
        }
        // Whatever the interleaving, it settles: make it finish and check the result.
        *e.srv.state.chunk_delay.lock().unwrap() = Duration::ZERO;
        let t = Instant::now();
        loop {
            let s = e.m.get_download(d.id).await.unwrap();
            match s.status {
                Completed => break,
                Paused => drop(e.m.resume_download(d.id).await),
                Failed => panic!("round {round}: failed: {:?}", s.error),
                _ => {}
            }
            assert!(t.elapsed() < Duration::from_secs(30), "round {round}: stuck in {:?}", s.status);
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(std::fs::read(e.root.join("race.bin")).unwrap(), *e.data, "round {round}: corrupt file after racing commands");
        let leftovers: Vec<_> = std::fs::read_dir(&e.root).unwrap().flatten().map(|f| f.file_name()).collect();
        assert_eq!(leftovers.len(), 1, "round {round}: {leftovers:?}");
    }
}

/// "Download later" parks a download in the queue without starting it. The UI must be able to tell
/// (to offer Start instead of a Pause that does nothing), and Start / Resume must run it to the end.
#[tokio::test]
async fn a_download_added_for_later_says_it_is_idle_and_starts_on_request() {
    let e = env("later").await;
    let d = e.m.create_download(NewDownload { url: e.srv.url("/later.bin"), start: Some(false), ..Default::default() }).await.unwrap();
    assert_eq!(d.status, Queued);
    assert!(d.idle, "a download added for later is idle");
    tokio::time::sleep(Duration::from_millis(200)).await;
    let still = e.m.get_download(d.id).await.unwrap();
    assert!(still.status == Queued && still.idle && still.downloaded == 0, "nothing starts it by itself: {still:?}");

    // Resume (the button on the Active page) starts it, and it is no longer idle.
    let started = e.m.resume_download(d.id).await.unwrap();
    assert!(!started.idle, "a started download is not idle");
    wait_status(&e.m, d.id, Completed).await;
    assert_eq!(std::fs::read(e.root.join("later.bin")).unwrap(), *e.data);

    // Start works the same; a running download is never idle.
    *e.srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(25);
    let d2 = e.m.create_download(NewDownload { url: e.srv.url("/later2.bin"), start: Some(false), ..Default::default() }).await.unwrap();
    e.m.start_download(d2.id).await.unwrap();
    wait_status(&e.m, d2.id, Downloading).await;
    assert!(!e.m.get_download(d2.id).await.unwrap().idle);
    e.m.cancel_download(d2.id).await.unwrap();

    // Pausing one that never started parks it: Paused, then Resume runs it.
    *e.srv.state.chunk_delay.lock().unwrap() = Duration::ZERO;
    let d3 = e.m.create_download(NewDownload { url: e.srv.url("/later3.bin"), start: Some(false), ..Default::default() }).await.unwrap();
    let paused = e.m.pause_download(d3.id).await.unwrap();
    assert_eq!(paused.status, Paused);
    assert!(!paused.idle);
    e.m.resume_download(d3.id).await.unwrap();
    wait_status(&e.m, d3.id, Completed).await;
}
