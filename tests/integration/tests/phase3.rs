use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use downloader_core::{
    AdaptiveConfig, ClientConfig, DownloadConfig, DownloadControl, DownloadRequest, DownloadStatus, DownloadStore, Downloader, Priority,
    ProgressHandle, RateLimits, RetryPolicy, SegmentStatus, SlotScheduler,
};
use persistence::SqliteStore;
use velox_integration_tests::{random_data, serve};

const MB: usize = 1024 * 1024;

fn tmpdir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("velox-p3-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

fn cfg() -> DownloadConfig {
    DownloadConfig {
        min_segment_size: 64 * 1024,
        retry: RetryPolicy {
            max_retries: 3,
            max_throttle_retries: 3,
            base_delay: Duration::from_millis(5),
            max_delay: Duration::from_millis(20),
            max_retry_after: Duration::from_millis(50),
        },
        adaptive: AdaptiveConfig { interval: Duration::from_millis(150), ..Default::default() },
        ..Default::default()
    }
}

fn downloader(cfg: DownloadConfig) -> Downloader {
    Downloader::new(&ClientConfig::default(), cfg).unwrap()
}

async fn run(d: &Downloader, url: String, dir: &PathBuf) -> downloader_core::DownloadOutcome {
    d.download(&DownloadRequest::new(url, dir), DownloadControl::new(), ProgressHandle::new()).await.unwrap()
}

// ----------------------------------------------------------- 3.1 adaptive connections

#[tokio::test]
async fn adaptive_ramps_up_when_the_server_limits_each_connection() {
    let data = random_data(24 * MB, 1);
    let srv = serve(data.clone()).await;
    // ~6.4 MB/s per connection: total throughput scales with the number of connections.
    *srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(5);
    let dir = tmpdir("ramp");
    let o = run(&downloader(cfg()), srv.url("/f.bin"), &dir).await;
    assert_eq!(std::fs::read(&o.path).unwrap(), *data);
    assert!(o.connections >= 4, "should have ramped past the start of 2, peak = {}", o.connections);
    assert!(srv.state.peak_concurrent.load(Ordering::SeqCst) >= 4);
}

#[tokio::test]
async fn adaptivity_off_uses_the_size_based_default_from_the_start() {
    let data = random_data(12 * MB, 2);
    let srv = serve(data.clone()).await;
    let mut c = cfg();
    c.adaptive.enabled = false;
    let dir = tmpdir("fixed");
    let o = run(&downloader(c), srv.url("/f.bin"), &dir).await;
    assert_eq!(std::fs::read(&o.path).unwrap(), *data);
    assert!(o.connections <= 8, "12 MB -> default 8, got {}", o.connections);
}

#[tokio::test]
async fn max_connections_is_a_hard_ceiling() {
    let data = random_data(24 * MB, 3);
    let srv = serve(data.clone()).await;
    *srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(5);
    let mut c = cfg();
    c.max_connections = 3;
    let dir = tmpdir("ceiling");
    let o = run(&downloader(c), srv.url("/f.bin"), &dir).await;
    assert_eq!(std::fs::read(&o.path).unwrap(), *data);
    assert!(o.connections <= 3 && srv.state.peak_concurrent.load(Ordering::SeqCst) <= 3);
}

#[tokio::test]
async fn server_side_connection_limit_429_makes_it_back_off_instead_of_failing() {
    // Models real servers (e.g. Hetzner) that answer 429 when one client opens too many connections.
    let data = random_data(24 * MB, 20);
    let srv = serve(data.clone()).await;
    *srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(5);
    srv.state.max_concurrent.store(2, Ordering::SeqCst);
    let mut c = cfg();
    c.retry.max_throttle_retries = 6; // deliberately small: the fix must not rely on a huge budget
    c.adaptive.enabled = true;
    let dir = tmpdir("throttle429");
    let o = run(&downloader(c), srv.url("/f.bin"), &dir).await;
    assert_eq!(std::fs::read(&o.path).unwrap(), *data);
    assert!(srv.state.throttled.load(Ordering::SeqCst) > 0, "test setup: the server never throttled");
    assert!(srv.state.peak_concurrent.load(Ordering::SeqCst) <= 2);
}

#[tokio::test]
async fn a_connection_limited_server_gets_runs_of_parts_not_a_request_per_part() {
    // A server allowing 2 connections that refuses everything above: each connection carries a run of parts in one
    // request (a request per part, plus a refused request handing its slot to the next part, used to make dozens of
    // refusals in a row and seconds without a byte). Fewer requests, almost no refusals, the same exact file.
    let data = random_data(24 * MB, 22);
    let srv = serve(data.clone()).await;
    *srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(5);
    srv.state.keep_alive.store(true, Ordering::SeqCst);
    srv.state.max_concurrent.store(2, Ordering::SeqCst);
    let mut c = cfg();
    c.adaptive.enabled = true;
    let dir = tmpdir("runs429");
    let progress = ProgressHandle::new();
    let o = downloader(c).download(&DownloadRequest::new(srv.url("/f.bin"), &dir), DownloadControl::new(), progress.clone()).await.unwrap();
    assert_eq!(std::fs::read(&o.path).unwrap(), *data);
    let parts = progress.snapshot().segments.len();
    let (hits, refused) = (srv.state.hits.load(Ordering::SeqCst), srv.state.throttled.load(Ordering::SeqCst));
    assert!(parts >= 32, "test setup: many parts ({parts})");
    assert!(hits * 3 <= parts, "{hits} requests for {parts} parts");
    assert!(refused <= 6, "{refused} refused requests");
    assert!(srv.state.peak_concurrent.load(Ordering::SeqCst) <= 2);
}

#[tokio::test]
async fn runs_still_spread_the_last_parts_over_every_connection() {
    // Each connection is capped by the server (slow per connection), 8 allowed: runs must not leave one connection
    // with a long tail while the others sit idle. Close to the time 8 connections need, not a single one's.
    let data = random_data(24 * MB, 23);
    let srv = serve(data.clone()).await;
    *srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(8); // ~4 MB/s per connection
    srv.state.keep_alive.store(true, Ordering::SeqCst);
    let mut c = cfg();
    c.adaptive.enabled = false;
    c.max_connections = 8;
    let dir = tmpdir("runs-tail");
    let t = Instant::now();
    let o = run(&downloader(c), srv.url("/f.bin"), &dir).await;
    let took = t.elapsed();
    assert_eq!(std::fs::read(&o.path).unwrap(), *data);
    assert!(o.connections >= 6, "connections used: {}", o.connections);
    // One connection alone: ~6 s; eight: ~0.75 s. Generous for a loaded machine, far below one connection.
    assert!(took < Duration::from_millis(2500), "took {took:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cap_keeps_few_connections_holds_its_speed_and_never_stops_the_download() {
    // Many connections at full speed, then a cap far below (a game starts): only the connections the cap needs stay
    // (each extra one filled the system's buffers: bursts, the cap overshot by half for many seconds, then everything
    // stopped while the backlog was paid), the speed sits at the cap, bytes keep coming, and all come back when it
    // lifts.
    let data = random_data(160 * MB, 24);
    let srv = serve(data.clone()).await;
    srv.state.keep_alive.store(true, Ordering::SeqCst);
    *srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(20); // ~1.6 MB/s per connection
    let limits = RateLimits::new();
    let mut c = cfg();
    c.adaptive.interval = Duration::from_millis(250);
    c.max_connections = 16;
    let d = downloader(c).with_limits(limits.clone());
    let dir = tmpdir("cap-conns");
    let (p, ctl) = (ProgressHandle::new(), DownloadControl::new());
    let task = {
        let (d, p, ctl, url, dir) = (d.clone(), p.clone(), ctl.clone(), srv.url("/f.bin"), dir.clone());
        tokio::spawn(async move { d.download(&DownloadRequest::new(url, &dir), ctl, p).await })
    };
    let open = |p: &ProgressHandle| p.snapshot().segments.iter().filter(|s| s.status == SegmentStatus::Downloading).count();
    let t = Instant::now();
    while open(&p) < 8 {
        assert!(t.elapsed() < Duration::from_secs(10), "test setup: many connections at full speed ({})", open(&p));
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    tokio::time::sleep(Duration::from_millis(800)).await;
    let free = open(&p);
    const CAP: u64 = 2_000_000;
    limits.set_smart(Some(CAP));
    // Every 100 ms for 6 s: bytes keep coming, and the connections go down to what the cap needs.
    let mut samples = vec![(Instant::now(), p.received())];
    for _ in 0..60 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        samples.push((Instant::now(), p.received()));
    }
    let capped = open(&p);
    assert!(capped <= 4 && capped < free, "connections under the cap: {capped} (free: {free})");
    let still: Vec<u128> =
        samples.windows(6).filter(|w| w[5].1 == w[0].1).map(|w| w[0].0.duration_since(samples[0].0).as_millis()).collect();
    assert!(still.is_empty(), "no bytes for half a second at (ms): {still:?}");
    // Once settled (the last 4 s), the speed is the cap's, within a few percent.
    let (a, b) = (samples[20], samples[60]);
    let speed = (b.1 - a.1) as f64 / b.0.duration_since(a.0).as_secs_f64();
    assert!((speed - CAP as f64).abs() < CAP as f64 * 0.12, "speed under the cap: {speed:.0} B/s");
    limits.set_smart(None);
    let t = Instant::now();
    while open(&p) < free.min(8) {
        assert!(t.elapsed() < Duration::from_secs(5), "the connections came back: {}", open(&p));
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    ctl.cancel();
    let _ = task.await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_download_started_under_a_cap_keeps_few_connections_and_its_speed() {
    // A game already runs when the download starts: what one connection gets is not known, so it does not climb to
    // many connections under the cap (each one would fill the system's buffers above it).
    let data = random_data(64 * MB, 25);
    let srv = serve(data.clone()).await;
    srv.state.keep_alive.store(true, Ordering::SeqCst);
    *srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(20); // ~1.6 MB/s per connection
    let limits = RateLimits::new();
    const CAP: u64 = 1_000_000;
    limits.set_smart(Some(CAP));
    let mut c = cfg();
    c.adaptive.interval = Duration::from_millis(250);
    let d = downloader(c).with_limits(limits.clone());
    let dir = tmpdir("cap-from-start");
    let (p, ctl) = (ProgressHandle::new(), DownloadControl::new());
    let task = {
        let (d, p, ctl, url, dir) = (d.clone(), p.clone(), ctl.clone(), srv.url("/f.bin"), dir.clone());
        tokio::spawn(async move { d.download(&DownloadRequest::new(url, &dir), ctl, p).await })
    };
    tokio::time::sleep(Duration::from_secs(2)).await;
    let (a, ta) = (p.received(), Instant::now());
    let mut most = 0;
    for _ in 0..30 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        most = most.max(p.snapshot().segments.iter().filter(|s| s.status == SegmentStatus::Downloading).count());
    }
    let speed = (p.received() - a) as f64 / ta.elapsed().as_secs_f64();
    assert!(most <= 3, "connections under the cap: {most}");
    assert!((speed - CAP as f64).abs() < CAP as f64 * 0.15, "speed under the cap: {speed:.0} B/s");
    ctl.cancel();
    let _ = task.await;
}

// --------------------------------------------------------------- 3.2 bandwidth limiter

#[tokio::test]
async fn per_download_limit_paces_the_transfer() {
    let data = random_data(MB, 4);
    let srv = serve(data.clone()).await;
    let d = downloader(cfg());
    let dir = tmpdir("perdl");
    let mut req = DownloadRequest::new(srv.url("/f.bin"), &dir);
    req.speed_limit = Some(512 * 1024); // 1 MiB at 512 KiB/s ~ 1.5-2 s
    let t = Instant::now();
    let o = d.download(&req, DownloadControl::new(), ProgressHandle::new()).await.unwrap();
    let took = t.elapsed();
    assert_eq!(std::fs::read(&o.path).unwrap(), *data);
    assert!(took >= Duration::from_millis(1300), "limit not enforced: {took:?}");
    assert!(took <= Duration::from_secs(5), "far slower than the limit: {took:?}");
}

#[tokio::test]
async fn limit_can_be_lifted_while_the_download_runs() {
    let data = random_data(3 * MB, 5);
    let srv = serve(data.clone()).await;
    let d = downloader(cfg());
    let dir = tmpdir("lift");
    let control = DownloadControl::new();
    control.set_speed_limit(Some(128 * 1024)); // would take ~20 s
    let (d2, c2, url, dir2) = (d.clone(), control.clone(), srv.url("/f.bin"), dir.clone());
    let task = tokio::spawn(async move { d2.download(&DownloadRequest::new(url, &dir2), c2, ProgressHandle::new()).await });
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(!task.is_finished());
    let t = Instant::now();
    control.set_speed_limit(None);
    let o = task.await.unwrap().unwrap();
    assert!(t.elapsed() < Duration::from_secs(4), "still throttled after lifting: {:?}", t.elapsed());
    assert_eq!(std::fs::read(&o.path).unwrap(), *data);
}

#[tokio::test]
async fn global_limit_is_shared_by_concurrent_downloads() {
    let a = random_data(MB, 6);
    let b = random_data(MB, 7);
    let (sa, sb) = (serve(a.clone()).await, serve(b.clone()).await);
    let limits = RateLimits::new();
    limits.set_global(Some(1024 * 1024)); // 2 MiB total at 1 MiB/s shared
    let d = downloader(cfg()).with_limits(limits);
    let (dir_a, dir_b) = (tmpdir("ga"), tmpdir("gb"));
    let t = Instant::now();
    let (oa, ob) = tokio::join!(run(&d, sa.url("/a.bin"), &dir_a), run(&d, sb.url("/b.bin"), &dir_b));
    let took = t.elapsed();
    assert_eq!(std::fs::read(oa.path).unwrap(), *a);
    assert_eq!(std::fs::read(ob.path).unwrap(), *b);
    // Unshared they would each finish in ~1 s; sharing one 1 MiB/s bucket needs ~1.7 s+.
    assert!(took >= Duration::from_millis(1400), "global limit not shared: {took:?}");
}

#[tokio::test]
async fn category_limit_only_affects_its_category() {
    let data = random_data(MB, 8);
    let srv = serve(data.clone()).await;
    let limits = RateLimits::new();
    limits.set_category("slow", Some(512 * 1024));
    let d = downloader(cfg()).with_limits(limits);

    let mut fast = DownloadRequest::new(srv.url("/fast.bin"), tmpdir("catfast"));
    fast.category = Some("other".into());
    let t = Instant::now();
    d.download(&fast, DownloadControl::new(), ProgressHandle::new()).await.unwrap();
    assert!(t.elapsed() < Duration::from_millis(1000), "uncategorised traffic was throttled");

    let mut slow = DownloadRequest::new(srv.url("/slow.bin"), tmpdir("catslow"));
    slow.category = Some("slow".into());
    let t = Instant::now();
    d.download(&slow, DownloadControl::new(), ProgressHandle::new()).await.unwrap();
    assert!(t.elapsed() >= Duration::from_millis(1300), "category limit not applied: {:?}", t.elapsed());
}

// ------------------------------------------------------------------- 3.3 priority

#[tokio::test]
async fn scheduler_runs_higher_priority_downloads_first() {
    let srv = serve(random_data(2 * MB, 9)).await;
    // Slow enough that `first` still holds the slot when the others arrive, also where timers are
    // exact (Linux, macOS): Windows rounds short sleeps up to ~15.6 ms, which hid this.
    *srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(40);
    let sched = SlotScheduler::new(1);
    let d = downloader(cfg()).with_scheduler(sched.clone());
    let dir = tmpdir("prio");

    let done = Arc::new(std::sync::Mutex::new(Vec::<&'static str>::new()));
    let launch = |name: &'static str, prio: Priority| {
        let (d, url, dir, done) = (d.clone(), srv.url(&format!("/{name}.bin")), dir.clone(), done.clone());
        tokio::spawn(async move {
            let mut req = DownloadRequest::new(url, &dir);
            req.priority = prio;
            d.download(&req, DownloadControl::new(), ProgressHandle::new()).await.unwrap();
            done.lock().unwrap().push(name);
        })
    };
    let first = launch("first", Priority::Normal);
    tokio::time::sleep(Duration::from_millis(150)).await; // `first` now holds the only slot
    let low = launch("low", Priority::Low);
    tokio::time::sleep(Duration::from_millis(30)).await;
    let highest = launch("highest", Priority::Highest);
    tokio::time::sleep(Duration::from_millis(30)).await;
    let normal = launch("normal", Priority::Normal);
    for t in [first, low, highest, normal] {
        t.await.unwrap();
    }
    assert_eq!(*done.lock().unwrap(), vec!["first", "highest", "normal", "low"]);
    assert_eq!(sched.running(), 0);
}

#[tokio::test]
async fn queued_download_can_be_reprioritised_or_paused_while_waiting() {
    let srv = serve(random_data(2 * MB, 10)).await;
    // Slow enough that the first download still holds the only slot while the others queue up (at 10 ms a
    // chunk it sometimes finished in ~250 ms, before the checks below).
    *srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(40);
    let sched = SlotScheduler::new(1);
    let d = downloader(cfg()).with_scheduler(sched.clone());
    let dir = tmpdir("prio2");

    let blocker = {
        let (d, url, dir) = (d.clone(), srv.url("/block.bin"), dir.clone());
        tokio::spawn(async move { d.download(&DownloadRequest::new(url, &dir), DownloadControl::new(), ProgressHandle::new()).await })
    };
    tokio::time::sleep(Duration::from_millis(150)).await;

    // A queued download can be re-prioritised by its queue key...
    let progress = ProgressHandle::new();
    let (d2, url2, dir2, p2) = (d.clone(), srv.url("/queued.bin"), dir.clone(), progress.clone());
    let queued = tokio::spawn(async move {
        let mut req = DownloadRequest::new(url2, &dir2);
        req.priority = Priority::Low;
        d2.download(&req, DownloadControl::new(), p2).await
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(sched.queued(), 1);
    assert!(sched.set_priority(progress.queue_key().unwrap(), Priority::Highest));

    // ...and a waiting download can be paused without ever taking a slot.
    let control = DownloadControl::new();
    let (d3, url3, dir3, c3) = (d.clone(), srv.url("/paused.bin"), dir.clone(), control.clone());
    let waiting = tokio::spawn(async move { d3.download(&DownloadRequest::new(url3, &dir3), c3, ProgressHandle::new()).await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(sched.queued(), 2);
    control.pause();
    assert!(matches!(waiting.await.unwrap(), Err(downloader_core::DownloadError::Paused)));
    assert_eq!(sched.queued(), 1, "paused waiter left the queue");

    blocker.await.unwrap().unwrap();
    queued.await.unwrap().unwrap();
    assert!(dir.join("queued.bin").exists() && !dir.join("paused.bin").exists());
}

#[tokio::test]
async fn priority_category_and_limit_are_persisted() {
    let srv = serve(random_data(MB, 11)).await;
    let dir = tmpdir("persist");
    std::fs::create_dir_all(&dir).unwrap();
    let store = Arc::new(SqliteStore::open(dir.join("s.db")).unwrap());
    let d = downloader(cfg()).with_store(store.clone());
    let mut req = DownloadRequest::new(srv.url("/f.bin"), &dir);
    req.priority = Priority::High;
    req.category = Some("video".into());
    req.speed_limit = Some(50 * MB as u64);
    let o = d.download(&req, DownloadControl::new(), ProgressHandle::new()).await.unwrap();
    let rec = store.load(o.id.unwrap()).unwrap().unwrap();
    assert_eq!(rec.status, DownloadStatus::Completed);
    assert_eq!((rec.priority, rec.category.as_deref(), rec.speed_limit), (Priority::High, Some("video"), Some(50 * MB as u64)));
}

// ---------------------------------------------------------------- 3.4 connection pooling

#[tokio::test]
async fn connections_are_reused_across_requests_and_downloads() {
    let data = random_data(256 * 1024, 12);
    let srv = serve(data.clone()).await;
    srv.state.keep_alive.store(true, Ordering::SeqCst);
    let mut c = cfg();
    c.max_connections = 1; // one connection at a time: every request can reuse the same socket
    let d = downloader(c);
    for i in 0..5 {
        let o = run(&d, srv.url(&format!("/f{i}.bin")), &tmpdir(&format!("pool{i}"))).await;
        assert_eq!(std::fs::read(o.path).unwrap(), *data);
    }
    // 5 downloads = 15+ requests (HEAD + probe + data each). Without pooling that is 15+ TCP
    // connections; with it, essentially one.
    let conns = srv.state.connections.load(Ordering::SeqCst);
    assert!(conns <= 2, "expected connection reuse, server accepted {conns} connections");
}

#[tokio::test]
async fn without_keep_alive_every_request_needs_a_new_connection() {
    // Control experiment: proves the previous test measures reuse rather than something incidental.
    let srv = serve(random_data(64 * 1024, 13)).await;
    let d = downloader(cfg());
    run(&d, srv.url("/f.bin"), &tmpdir("nopool")).await;
    assert!(srv.state.connections.load(Ordering::SeqCst) >= 3);
}
