//! Several networks at once on the real computer, to a real https server (run by hand, needs two or more connected
//! networks): `cargo test -p velox-integration-tests --test multinet_real -- --ignored --nocapture`.
//! The networks are the ones the app finds itself; the system's own byte counters (`/proc/net/dev`, Linux) say
//! independently which adapters carried the file.
#![cfg(target_os = "linux")]

use std::collections::HashMap;
use std::time::{Duration, Instant};

use download_manager::{DownloadManager, ManagerConfig, NewDownload, SetMultiNetwork};
use downloader_core::DownloadStatus;

/// A CDN that serves every connection (no per-address limit), 145 MB.
const URL: &str = "https://cdn.kernel.org/pub/linux/kernel/v6.x/linux-6.10.tar.xz";
const SIZE: u64 = 145_142_812;

fn counters() -> HashMap<String, u64> {
    std::fs::read_to_string("/proc/net/dev")
        .unwrap()
        .lines()
        .skip(2)
        .filter_map(|l| {
            let (name, rest) = l.split_once(':')?;
            Some((name.trim().to_string(), rest.split_whitespace().next()?.parse().ok()?))
        })
        .collect()
}

async fn fetch(spread: bool, tag: &str) -> (f64, HashMap<String, u64>, Vec<(String, u64)>) {
    let base = std::env::temp_dir().join(format!("velox-multinet-real-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let m = DownloadManager::new(ManagerConfig::new(base.join("velox.db"), base.join("dl"))).await.unwrap();
    let dto = m.set_multi_network(SetMultiNetwork { enabled: spread, skip: vec![] }).await.unwrap();
    println!("[{tag}] networks found: {:?} blocked: {:?}", dto.networks.iter().map(|n| (&n.name, n.main)).collect::<Vec<_>>(), dto.blocked);
    let before = counters();
    let t = Instant::now();
    let d = m.create_download(NewDownload { url: URL.into(), ..Default::default() }).await.unwrap();
    loop {
        let x = m.get_download(d.id).await.unwrap();
        assert_ne!(x.status, DownloadStatus::Failed, "{:?}", x.error_detail);
        if x.status == DownloadStatus::Completed {
            break;
        }
        assert!(t.elapsed() < Duration::from_secs(600), "too slow");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let secs = t.elapsed().as_secs_f64();
    let after = counters();
    let used: HashMap<String, u64> =
        after.iter().map(|(k, v)| (k.clone(), v.saturating_sub(*before.get(k).unwrap_or(&0)))).filter(|(_, v)| *v > 1_000_000).collect();
    let diag = m.get_download_diagnostics(d.id).await.unwrap();
    let nets = diag.networks.iter().map(|n| (n.name.clone(), n.bytes)).collect();
    let size = std::fs::metadata(base.join("dl").join("linux-6.10.tar.xz")).unwrap().len();
    assert_eq!(size, SIZE, "whole file");
    println!(
        "[{tag}] {:.1} s, {:.2} MB/s; system counters (bytes in): {used:?}; app per network: {nets:?}",
        secs,
        size as f64 / secs / 1e6
    );
    let _ = std::fs::remove_dir_all(&base);
    (secs, used, nets)
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs two or more real networks and the internet"]
async fn a_real_download_is_spread_over_the_real_networks() {
    let (one, _, nets1) = fetch(false, "usual").await;
    assert!(nets1.is_empty(), "off: nothing spread");
    let (many, used, nets) = fetch(true, "spread").await;
    assert!(nets.len() >= 2 && nets.iter().all(|(_, b)| *b > 5_000_000), "the app used several networks: {nets:?}");
    for (name, _) in &nets {
        assert!(used.get(name).is_some_and(|b| *b > 5_000_000), "the system saw {name} carry bytes: {used:?}");
    }
    println!("usual route {one:.1} s, spread {many:.1} s ({:.2}x)", one / many);
    assert!(one / many >= 1.2, "spreading over the networks must be clearly faster: {one:.1} s -> {many:.1} s");
}

/// What each network does during a real spread download (states every 2 s, and the log lines about networks).
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs two or more real networks and the internet"]
async fn show_what_each_real_network_does() {
    let log = diagnostics::LogHandle::new(diagnostics::LogConfig { stderr: true, ..Default::default() });
    log.install();
    log.set_detailed(true);
    let base = std::env::temp_dir().join(format!("velox-multinet-diag-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let m = DownloadManager::new(ManagerConfig::new(base.join("velox.db"), base.join("dl"))).await.unwrap();
    m.set_multi_network(SetMultiNetwork { enabled: true, skip: vec![] }).await.unwrap();
    let d = m.create_download(NewDownload { url: URL.into(), ..Default::default() }).await.unwrap();
    let t = Instant::now();
    let mut last: HashMap<String, u64> = HashMap::new();
    let s_names: Vec<String> = m.get_multi_network().await.networks.into_iter().map(|n| n.name).collect();
    while t.elapsed() < Duration::from_secs(300) {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let x = m.get_download(d.id).await.unwrap();
        if x.status == DownloadStatus::Completed {
            eprintln!("DONE in {:.1} s ({:.2} MB/s)", t.elapsed().as_secs_f64(), SIZE as f64 / t.elapsed().as_secs_f64() / 1e6);
            break;
        }
        let now = counters();
        let rates: Vec<String> = s_names
            .iter()
            .map(|n| {
                format!(
                    "{n}={:.2}",
                    now.get(n).unwrap_or(&0).saturating_sub(*last.get(n).unwrap_or(&now.get(n).copied().unwrap_or(0))) as f64 / 2e6
                )
            })
            .collect();
        last = now;
        eprintln!("RATE MB/s {}", rates.join(" "));
        let s = m.get_multi_network().await;
        eprintln!(
            "STATUS {:>3}s {:?} {:.2} MB/s conns={} | {:?}",
            t.elapsed().as_secs(),
            x.status,
            x.speed_bps as f64 / 1e6,
            x.connections,
            s.networks.iter().map(|n| (n.name.as_str(), n.kind, n.state, n.active, n.bytes / 1_000_000)).collect::<Vec<_>>()
        );
    }
    let _ = m.cancel_download(d.id).await;
    let _ = std::fs::remove_dir_all(&base);
}

/// Turns a network off (`nmcli device disconnect`) and back on when dropped (without waiting: the download is watched
/// meanwhile); `reconnect_all` waits for every network at the end, even when the test fails.
struct Unplugged(String);

impl Unplugged {
    fn new(dev: &str) -> Self {
        let ok = std::process::Command::new("nmcli").args(["device", "disconnect", dev]).status().is_ok_and(|s| s.success());
        assert!(ok, "could not disconnect {dev}");
        Unplugged(dev.to_string())
    }
}

impl Drop for Unplugged {
    fn drop(&mut self) {
        let _ = std::process::Command::new("nmcli")
            .args(["device", "connect", &self.0])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
}

/// Every one of `devs` connected again (at most 60 s).
fn reconnect_all(devs: &[String]) {
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(60) {
        let out = std::process::Command::new("nmcli").args(["-t", "-f", "DEVICE,STATE", "device"]).output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        let down: Vec<&String> = devs.iter().filter(|d| !text.lines().any(|l| l == format!("{d}:connected"))).collect();
        if down.is_empty() {
            return;
        }
        for d in down {
            let _ = std::process::Command::new("nmcli").args(["device", "connect", d]).status();
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

/// While a large download runs over every network, each network is disconnected, then connected again: the download
/// never fails nor stops (the others carry it at once), and a network that comes back is used again.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs two or more real networks managed by NetworkManager, and the internet; disconnects them in turn"]
async fn switching_between_real_networks_when_one_goes_or_comes_back() {
    const BIG: &str = "https://mirrors.edge.kernel.org/debian-cd/current/amd64/iso-dvd/debian-13.7.0-amd64-DVD-1.iso";
    let base = std::env::temp_dir().join(format!("velox-multinet-switch-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let m = DownloadManager::new(ManagerConfig::new(base.join("velox.db"), base.join("dl"))).await.unwrap();
    m.set_multi_network(SetMultiNetwork { enabled: true, skip: vec![] }).await.unwrap();
    let nets: Vec<(String, String)> =
        m.get_multi_network().await.networks.into_iter().map(|n| (n.name.clone(), format!("{:?}", n.kind))).collect();
    assert!(nets.len() >= 2, "{nets:?}");
    let d = m.create_download(NewDownload { url: BIG.into(), ..Default::default() }).await.unwrap();
    let t = Instant::now();
    let mut slowest_gap = Duration::ZERO;
    let mut last_bytes = 0u64;
    let mut last_move = Instant::now();
    // Watch for `secs`: the download keeps moving (never more than 4 s without a byte), never fails.
    let mut watch = async |what: &str, secs: u64| -> HashMap<String, u64> {
        let until = Instant::now() + Duration::from_secs(secs);
        let before = counters();
        let app =
            async || -> HashMap<String, u64> { m.get_multi_network().await.networks.into_iter().map(|n| (n.name, n.bytes)).collect() };
        let app_before = app().await;
        while Instant::now() < until {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let x = m.get_download(d.id).await.unwrap();
            assert_ne!(x.status, DownloadStatus::Failed, "{what}: {:?}", x.error_detail);
            if x.downloaded > last_bytes {
                slowest_gap = slowest_gap.max(last_move.elapsed());
                last_bytes = x.downloaded;
                last_move = Instant::now();
            }
            assert!(last_move.elapsed() < Duration::from_secs(4), "{what}: the download stopped");
            slowest_gap = slowest_gap.max(last_move.elapsed());
        }
        let after = counters();
        let app_after = app().await;
        let mb = |b: u64| (b as f64 / secs as f64 / 1e6 * 100.0).round() / 100.0;
        let system: Vec<(&str, f64)> =
            nets.iter().map(|(n, _)| (n.as_str(), mb(after.get(n).unwrap_or(&0).saturating_sub(*before.get(n).unwrap_or(&0))))).collect();
        // What the app received through each network (a network that went is no longer listed: 0).
        let per: HashMap<String, u64> = nets
            .iter()
            .map(|(n, _)| (n.clone(), app_after.get(n).map_or(0, |a| a.saturating_sub(*app_before.get(n).unwrap_or(&0)))))
            .collect();
        let total: u64 = per.values().sum();
        let dto = m.get_multi_network().await;
        eprintln!(
            "{what:<34} [{:?}, blocked {:?}] app {:>5.2} MB/s {:?} | system {:?}",
            dto.networks.iter().map(|n| (n.kind, n.state)).collect::<Vec<_>>(),
            dto.blocked,
            mb(total),
            per.iter().map(|(n, b)| (n.as_str(), mb(*b))).collect::<Vec<_>>(),
            system
        );
        per
    };
    let all = watch("all networks", 12).await;
    assert!(nets.iter().all(|(n, _)| all[n] > 1_000_000), "every network in use: {all:?}");
    for (name, kind) in nets.clone() {
        let gone = Unplugged::new(&name);
        // The bytes already on their way when it went are still counted for a moment (the download is watched meanwhile).
        watch(&format!("{kind} going"), 2).await;
        let without = watch(&format!("{kind} disconnected"), 15).await;
        assert!(without[&name] < 500_000, "nothing through a disconnected network");
        drop(gone);
        // NetworkManager brings it back (an address in a few seconds); the app reads the networks every 15 s.
        let back = watch(&format!("{kind} connected again"), 30).await;
        assert!(back[&name] > 3_000_000, "{kind} used again once it is back: {back:?}");
    }
    eprintln!(
        "longest pause without a byte: {:.1} s; {} MB in {:.0} s",
        slowest_gap.as_secs_f64(),
        last_bytes / 1_000_000,
        t.elapsed().as_secs_f64()
    );
    reconnect_all(&nets.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>());
    let _ = m.cancel_download(d.id).await;
    let _ = std::fs::remove_dir_all(&base);
}

/// Every 2 s, each network's state, connections and speed while one network (`VELOX_TEST_UNPLUG`, a device name) is
/// disconnected at 10 s and connected again at 25 s, with the log lines about networks.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs real networks managed by NetworkManager; disconnects one"]
async fn show_one_network_going_and_coming_back() {
    let dev = std::env::var("VELOX_TEST_UNPLUG").expect("VELOX_TEST_UNPLUG=<device>");
    const BIG: &str = "https://mirrors.edge.kernel.org/debian-cd/current/amd64/iso-dvd/debian-13.7.0-amd64-DVD-1.iso";
    let log = diagnostics::LogHandle::new(diagnostics::LogConfig { stderr: true, ..Default::default() });
    log.install();
    log.set_detailed(true);
    let base = std::env::temp_dir().join(format!("velox-multinet-unplug-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let m = DownloadManager::new(ManagerConfig::new(base.join("velox.db"), base.join("dl"))).await.unwrap();
    m.set_multi_network(SetMultiNetwork { enabled: true, skip: vec![] }).await.unwrap();
    let d = m.create_download(NewDownload { url: BIG.into(), ..Default::default() }).await.unwrap();
    let t = Instant::now();
    let mut gone: Option<Unplugged> = None;
    let mut last: HashMap<String, u64> = HashMap::new();
    while t.elapsed() < Duration::from_secs(70) {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let s = t.elapsed().as_secs();
        if (10..12).contains(&s) && gone.is_none() {
            gone = Some(Unplugged::new(&dev));
            eprintln!("---- {dev} disconnected");
        }
        if (25..27).contains(&s) && gone.is_some() {
            gone = None;
            eprintln!("---- {dev} connecting again");
        }
        let x = m.get_download(d.id).await.unwrap();
        let n = m.get_multi_network().await;
        let per: Vec<String> = n
            .networks
            .iter()
            .map(|l| {
                let d = l.bytes.saturating_sub(*last.get(&l.name).unwrap_or(&l.bytes));
                format!("{:?}:{:?}:{}c:{:.2}", l.kind, l.state, l.active, d as f64 / 2e6)
            })
            .collect();
        last = n.networks.iter().map(|l| (l.name.clone(), l.bytes)).collect();
        eprintln!(
            "STATUS {s:>3}s {:.2} MB/s conns={} blocked={:?} | {}",
            x.speed_bps as f64 / 1e6,
            x.connections,
            n.blocked,
            per.join("  ")
        );
    }
    drop(gone);
    let _ = m.cancel_download(d.id).await;
    reconnect_all(&[dev]);
    let _ = std::fs::remove_dir_all(&base);
}
