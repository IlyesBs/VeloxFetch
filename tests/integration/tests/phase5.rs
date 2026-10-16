//! Phase 5 backend: metrics aggregator (batched ticks), dashboard metrics, detail view data, history.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use download_manager::{DownloadManager, ManagerConfig, ManagerError, ManagerEvent, NewDownload, TickDto};
use downloader_core::{AdaptiveConfig, DownloadStatus, RetryPolicy, SegmentStatus};
use velox_integration_tests::{random_data, serve, Server};

const MB: usize = 1024 * 1024;

fn paths(name: &str) -> (PathBuf, PathBuf) {
    let base = std::env::temp_dir().join(format!("velox-p5-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    (base.join("state").join("velox.db"), base.join("downloads"))
}

/// Default tick interval (200 ms) on purpose: that is what the UI gets.
async fn manager(name: &str, slots: usize) -> DownloadManager {
    let (db, root) = paths(name);
    let mut c = ManagerConfig::new(db, root);
    c.max_concurrent_downloads = slots;
    c.download.min_segment_size = 64 * 1024;
    c.download.checkpoint_interval = Duration::from_millis(50);
    c.download.retry = RetryPolicy { max_retries: 1, max_throttle_retries: 1, ..Default::default() };
    c.download.adaptive = AdaptiveConfig { interval: Duration::from_millis(150), ..Default::default() };
    DownloadManager::new(c).await.unwrap()
}

fn dl(srv: &Server, path: &str) -> NewDownload {
    NewDownload { url: srv.url(path), ..Default::default() }
}

async fn wait_status(m: &DownloadManager, id: i64, want: DownloadStatus) {
    let t = Instant::now();
    loop {
        let d = m.get_download(id).await.unwrap();
        if d.status == want {
            return;
        }
        assert!(t.elapsed() < Duration::from_secs(20), "stuck in {:?}", d.status);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_bytes(m: &DownloadManager, id: i64, bytes: u64) {
    let t = Instant::now();
    while m.get_download(id).await.unwrap().downloaded < bytes {
        assert!(t.elapsed() < Duration::from_secs(20), "never reached {bytes}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn drain_ticks(rx: &mut tokio::sync::broadcast::Receiver<ManagerEvent>) -> (Vec<TickDto>, usize) {
    let (mut ticks, mut others) = (Vec::new(), 0);
    while let Ok(ev) = rx.try_recv() {
        match ev {
            ManagerEvent::Tick { tick } => ticks.push(tick),
            _ => others += 1,
        }
    }
    (ticks, others)
}

#[tokio::test]
async fn one_batched_tick_covers_every_running_download_at_5_to_10_hz() {
    let srv = serve(random_data(12 * MB, 1)).await;
    let m = manager("batch", 3).await;
    let mut rx = m.subscribe();

    let ids: Vec<i64> = {
        let mut v = Vec::new();
        for n in 0..3 {
            // Pinned to 2 MiB/s so none of them can finish inside the measuring window.
            let mut req = dl(&srv, &format!("/f{n}.bin"));
            req.speed_limit = Some(2 * MB as u64);
            v.push(m.create_download(req).await.unwrap().id);
        }
        v
    };
    for id in &ids {
        wait_bytes(&m, *id, 200_000).await;
    }
    let _ = drain_ticks(&mut rx); // start measuring once all three are flowing
    let t = Instant::now();
    tokio::time::sleep(Duration::from_millis(2000)).await;
    let (ticks, _) = drain_ticks(&mut rx);
    let per_sec = ticks.len() as f64 / t.elapsed().as_secs_f64();
    assert!((4.0..=11.0).contains(&per_sec), "expected 5-10 ticks/s, got {per_sec:.1} ({} ticks)", ticks.len());

    // Each tick is ONE event carrying all three downloads (not one event per download).
    let full = ticks.iter().filter(|k| k.downloads.len() == 3).count();
    assert!(full * 10 >= ticks.len() * 9, "ticks should batch all running downloads: {full}/{}", ticks.len());
    // Values are sane and monotonic per download.
    for id in &ids {
        let series: Vec<u64> = ticks.iter().filter_map(|k| k.downloads.iter().find(|p| p.id == *id)).map(|p| p.downloaded).collect();
        assert!(series.windows(2).all(|w| w[0] <= w[1]), "bytes must never go backwards: {series:?}");
    }
    let last = ticks.last().unwrap();
    assert_eq!(last.metrics.active, 3);
    let summed: u64 = last.downloads.iter().map(|p| p.speed_bps).sum();
    assert_eq!(last.metrics.speed_bps, summed, "global speed is the sum of the running downloads");
    assert!(last.metrics.speed_bps > 500_000, "speed {}", last.metrics.speed_bps);
    assert!(last.downloads.iter().all(|p| p.connections >= 1 && p.total_size == Some(12 * MB as u64)));
    for id in ids {
        m.cancel_download(id).await.unwrap();
    }
}

#[tokio::test]
async fn ticks_stop_when_idle_after_settling_on_zero() {
    let srv = serve(random_data(MB, 2)).await;
    let m = manager("idle", 3).await;
    let mut rx = m.subscribe();
    let d = m.create_download(dl(&srv, "/a.bin")).await.unwrap();
    wait_status(&m, d.id, DownloadStatus::Completed).await;
    tokio::time::sleep(Duration::from_millis(900)).await; // settling ticks
    let (ticks, _) = drain_ticks(&mut rx);
    assert!(!ticks.is_empty());
    let last = ticks.last().unwrap();
    assert_eq!((last.metrics.speed_bps, last.metrics.active, last.downloads.len()), (0, 0, 0), "UI must settle on zeros");
    tokio::time::sleep(Duration::from_millis(700)).await;
    let (quiet, others) = drain_ticks(&mut rx);
    assert_eq!((quiet.len(), others), (0, 0), "no events at all while idle");
}

#[tokio::test]
async fn metrics_reflect_queue_history_network_and_disk() {
    let srv = serve(random_data(2 * MB, 3)).await;
    // Slow enough that the first download is still running when the queue is looked at (it starts on 4 connections).
    *srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(60);
    let m = manager("metrics", 1).await;

    let first = m.create_download(dl(&srv, "/first.bin")).await.unwrap();
    wait_status(&m, first.id, DownloadStatus::Downloading).await;
    let _q1 = m.create_download(dl(&srv, "/q1.bin")).await.unwrap();
    let _q2 = m.create_download(dl(&srv, "/q2.bin")).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let live = m.get_metrics().await.unwrap();
    assert_eq!((live.active, live.queued), (1, 2), "one slot: 1 running, 2 waiting");
    assert!(live.disk_total.is_some_and(|t| t > 0) || live.disk_total.is_none(), "disk numbers arrive via the background refresh");

    *srv.state.chunk_delay.lock().unwrap() = Duration::ZERO;
    for id in [first.id, first.id + 1, first.id + 2] {
        wait_status(&m, id, DownloadStatus::Completed).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    let done = m.get_metrics().await.unwrap();
    assert_eq!((done.completed, done.active, done.queued), (3, 0, 0));
    assert_eq!(done.total_downloaded, 6 * MB as u64, "lifetime total = sum of completed sizes");
    assert!(
        done.session_downloaded >= 5 * MB as u64 && done.session_downloaded <= 7 * MB as u64,
        "session bytes {}",
        done.session_downloaded
    );
    // Free / total space of the download volume are reported (refreshed in the background).
    let disk = loop {
        let mm = m.get_metrics().await.unwrap();
        if mm.disk_total.is_some() {
            break mm;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert!(disk.disk_total.unwrap() > 0 && disk.disk_free.unwrap() <= disk.disk_total.unwrap());
}

#[tokio::test]
async fn disk_space_is_known_before_the_download_folder_exists() {
    // A fresh install: the download folder is only created by the first download. The dashboard used to
    // say "reading disk…" forever, because the free space of a missing folder cannot be read.
    let m = manager("fresh-disk", 1).await;
    let root = paths("fresh-disk").1;
    let _ = std::fs::remove_dir_all(&root);
    let t = Instant::now();
    let mm = loop {
        let mm = m.get_metrics().await.unwrap();
        if mm.disk_total.is_some() && mm.folder_bytes.is_some() {
            break mm;
        }
        assert!(t.elapsed() < Duration::from_secs(10), "disk numbers never arrived: {mm:?}");
        // The refresh runs at most every 5 s; the first one starts at once.
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert!(mm.disk_total.unwrap() > 0 && mm.disk_free.unwrap() <= mm.disk_total.unwrap());
    assert_eq!(mm.folder_bytes, Some(0));
}

#[tokio::test]
async fn detail_view_data_for_pending_running_paused_and_completed() {
    let data = random_data(8 * MB, 4);
    let srv = serve(data.clone()).await;
    *srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(15);
    let m = manager("detail", 3).await;

    // Not started: nothing planned yet.
    let mut req = dl(&srv, "/d.bin");
    req.start = Some(false);
    let d = m.create_download(req).await.unwrap();
    let pending = m.get_download_detail(d.id).await.unwrap();
    assert_eq!((pending.mode.as_str(), pending.segments.len(), pending.server.clone()), ("pending", 0, None));

    // Running: chunks, average speed, server metadata.
    m.start_download(d.id).await.unwrap();
    wait_bytes(&m, d.id, 2 * MB as u64).await;
    let run = m.get_download_detail(d.id).await.unwrap();
    assert_eq!(run.mode, "parallel");
    assert!(run.segments.len() >= 4, "many chunks: {}", run.segments.len());
    assert_eq!(run.segments.iter().map(|s| s.end - s.start + 1).sum::<u64>(), data.len() as u64, "chunks tile the file");
    assert!(run.segments.windows(2).all(|w| w[0].end + 1 == w[1].start));
    assert!(run.segments.iter().any(|s| s.status == SegmentStatus::Downloading));
    let seg_sum: u64 = run.segments.iter().map(|s| s.downloaded).sum();
    assert!(seg_sum.abs_diff(run.download.downloaded) < 512 * 1024, "{seg_sum} vs {}", run.download.downloaded);
    assert!(run.avg_speed_bps > 0 && run.elapsed_secs.is_some());
    assert_eq!(run.server.as_deref(), Some("velox-test/1.0"));
    assert_eq!(run.content_type.as_deref(), Some("application/octet-stream"));
    assert_eq!(run.etag.as_deref(), Some("\"v1\""));
    assert_eq!(run.remaining, Some(data.len() as u64 - run.download.downloaded));

    // Paused: the saved chunks are still shown.
    m.pause_download(d.id).await.unwrap();
    let paused = m.get_download_detail(d.id).await.unwrap();
    assert_eq!((paused.mode.as_str(), paused.avg_speed_bps), ("parallel", 0));
    assert_eq!(paused.segments.len(), run.segments.len());
    assert!(
        paused.segments.iter().any(|s| s.status == SegmentStatus::Completed)
            && paused.segments.iter().any(|s| s.status != SegmentStatus::Completed)
    );

    // Completed: every chunk done.
    *srv.state.chunk_delay.lock().unwrap() = Duration::ZERO;
    m.resume_download(d.id).await.unwrap();
    wait_status(&m, d.id, DownloadStatus::Completed).await;
    let done = m.get_download_detail(d.id).await.unwrap();
    assert!(done.segments.iter().all(|s| s.status == SegmentStatus::Completed && s.downloaded == s.end - s.start + 1));
    assert_eq!(done.remaining, Some(0));

    assert!(matches!(m.get_download_detail(9999).await, Err(ManagerError::NotFound(9999))));
}

#[tokio::test]
async fn history_lists_completed_downloads_newest_first() {
    let srv = serve(random_data(500_000, 5)).await;
    let m = manager("history", 3).await;
    assert!(m.list_history(50).await.unwrap().is_empty());
    let mut names = Vec::new();
    for n in 0..3 {
        let name = format!("h{n}.bin");
        let d = m.create_download(dl(&srv, &format!("/{name}"))).await.unwrap();
        wait_status(&m, d.id, DownloadStatus::Completed).await;
        names.push(name);
        tokio::time::sleep(Duration::from_millis(1100)).await; // distinct second-resolution timestamps
    }
    // A cancelled download never becomes history.
    let mut req = dl(&srv, "/never.bin");
    req.start = Some(false);
    let c = m.create_download(req).await.unwrap();
    m.cancel_download(c.id).await.unwrap();

    let h = m.list_history(50).await.unwrap();
    assert_eq!(h.iter().map(|e| e.filename.as_str()).collect::<Vec<_>>(), vec!["h2.bin", "h1.bin", "h0.bin"]);
    assert!(h.iter().all(|e| e.outcome == "completed" && e.size == Some(500_000) && e.finished_at > 0));
    assert_eq!(m.list_history(2).await.unwrap().len(), 2);
    assert_eq!(m.list_history(0).await.unwrap().len(), 1, "limit is clamped to at least 1");
}

#[tokio::test]
async fn network_usage_is_kept_after_deleting_downloads_clearing_history_and_restarting() {
    // Issue #45: the dashboard's network usage went back to 0 after an update, a reinstall or deleting the downloads.
    use download_manager::Clock;
    let srv = serve(random_data(2 * MB, 9)).await;
    let (db, root) = paths("usage");
    let at = |s: &'static str| {
        Clock(std::sync::Arc::new(move || scheduler::chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap()))
    };
    let open = |clock: Clock| {
        let mut c = ManagerConfig::new(db.clone(), root.clone());
        c.clock = clock;
        c.download.min_segment_size = 64 * 1024;
        DownloadManager::new(c)
    };
    let m = open(at("2026-10-31 23:50")).await.unwrap();
    let d = m.create_download(dl(&srv, "/a.bin")).await.unwrap();
    wait_status(&m, d.id, DownloadStatus::Completed).await;
    let mm = m.get_metrics().await.unwrap();
    assert!(mm.network_total >= 2 * MB as u64 && mm.network_today == mm.network_total && mm.network_month == mm.network_total, "{mm:?}");
    m.delete_download(d.id, true).await.unwrap();
    m.forget_history(None).await.unwrap();
    assert_eq!(m.get_metrics().await.unwrap().network_total, mm.network_total, "deleting downloads and the history keeps it");
    m.shutdown().await;
    drop(m);

    // Opened again (an update or a reinstall keeps the database), on the next day of the next month.
    let m = open(at("2026-11-01 00:10")).await.unwrap();
    let again = m.get_metrics().await.unwrap();
    assert_eq!(
        (again.network_total, again.network_today, again.network_month, again.session_downloaded),
        (mm.network_total, 0, 0, 0),
        "{again:?}"
    );
    let d = m.create_download(dl(&srv, "/b.bin")).await.unwrap();
    wait_status(&m, d.id, DownloadStatus::Completed).await;
    let now = m.get_metrics().await.unwrap();
    assert!(
        now.network_total >= mm.network_total + 2 * MB as u64
            && now.network_today >= 2 * MB as u64
            && now.network_month == now.network_today,
        "{now:?}"
    );
    m.shutdown().await;
}
