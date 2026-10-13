//! Several networks at once (`http_client::lanes`, `manager/multinet.rs`): a download's connections are spread over
//! the chosen networks and the file is intact; a network that fails, or that a server refuses, never fails a
//! download; nothing is spread when it is off, with one network, through a proxy, or when the system reaches the
//! server another way (a VPN).
//!
//! The "networks" are two addresses of this computer's loopback (`127.0.0.2`, `127.0.0.3`): the test server sees
//! which one each request came from. The app itself never spreads plain-http or local links; the tests allow it
//! (`spread_any_link`), the rest is the app's own path.
#![cfg(any(target_os = "linux", windows))] // macOS has only 127.0.0.1 on its loopback

use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use download_manager::{
    DownloadDto, DownloadManager, LaneState, ManagerConfig, MultiNetworkDto, NewDownload, ProxyMode, RouteSpec, SetMultiNetwork,
    SetNetwork, SetSmartThrottle,
};
use downloader_core::DownloadStatus;
use velox_integration_tests::{random_data, serve, Server};

const MB: usize = 1024 * 1024;
const A: Ipv4Addr = Ipv4Addr::new(127, 0, 0, 2);
const B: Ipv4Addr = Ipv4Addr::new(127, 0, 0, 3);
/// An address this computer does not have: binding to it fails (a network that went away).
const GONE: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 77);

struct Env {
    root: PathBuf,
    db: PathBuf,
}

impl Drop for Env {
    fn drop(&mut self) {
        if let Some(base) = self.root.parent() {
            let _ = std::fs::remove_dir_all(base);
        }
    }
}

fn env(name: &str) -> Env {
    let base = std::env::temp_dir().join(format!("velox-multinet-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    Env { root: base.join("downloads"), db: base.join("state").join("velox.db") }
}

/// A network bound to `ip` on the loopback. `reaches_server`: the system's own route to the test server leaves from
/// one of its addresses (`false`: like a VPN carrying it).
fn net(name: &str, ip: Ipv4Addr, reaches_server: bool) -> RouteSpec {
    let mut addresses = vec![IpAddr::V4(ip)];
    if reaches_server {
        addresses.push(IpAddr::V4(Ipv4Addr::LOCALHOST));
    }
    let interface = if cfg!(windows) { None } else { Some("lo".to_string()) };
    RouteSpec { name: name.into(), interface, v4: Some(ip), v6: None, addresses }
}

async fn manager(e: &Env, nets: Vec<RouteSpec>) -> DownloadManager {
    let mut c = ManagerConfig::new(&e.db, &e.root);
    c.progress_interval = Duration::from_millis(40);
    c.queue_poll = Duration::from_millis(30);
    c.download.min_segment_size = 128 * 1024;
    c.client.spread_any_link = true;
    c.test_networks = Some(nets);
    DownloadManager::new(c).await.unwrap()
}

async fn until(what: &str, secs: u64, mut f: impl AsyncFnMut() -> bool) {
    let t = Instant::now();
    while !f().await {
        assert!(t.elapsed() < Duration::from_secs(secs), "timed out waiting for: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn get(m: &DownloadManager, id: i64) -> DownloadDto {
    m.get_download(id).await.unwrap()
}

async fn on(m: &DownloadManager, skip: &[&str]) -> MultiNetworkDto {
    m.set_multi_network(SetMultiNetwork { enabled: true, skip: skip.iter().map(|s| s.to_string()).collect() }).await.unwrap()
}

/// Download `path` from `srv` to the end; its bytes are checked against `data`.
async fn download(m: &DownloadManager, e: &Env, srv: &Server, path: &str, data: &[u8]) -> DownloadDto {
    let d = m.create_download(NewDownload { url: srv.url(path), ..Default::default() }).await.unwrap();
    until("completed", 60, async || {
        let x = get(m, d.id).await;
        assert_ne!(x.status, DownloadStatus::Failed, "{:?}", x.error_detail);
        x.status == DownloadStatus::Completed
    })
    .await;
    let name = path.rsplit('/').next().unwrap();
    assert!(std::fs::read(e.root.join(name)).unwrap() == data, "the file is intact");
    get(m, d.id).await
}

fn served_to(srv: &Server, ip: Ipv4Addr) -> u64 {
    srv.state.bytes_by_peer.lock().unwrap().get(&IpAddr::V4(ip)).copied().unwrap_or(0)
}

/// Data bytes per network (the server check, a few bytes, always goes the normal route).
fn shares(srv: &Server) -> (u64, u64, u64) {
    (served_to(srv, A), served_to(srv, B), served_to(srv, Ipv4Addr::LOCALHOST))
}

#[tokio::test]
async fn a_download_is_spread_over_the_networks_and_the_file_is_intact() {
    let data = random_data(12 * MB, 501);
    let srv = serve(data.clone()).await;
    // Each connection is slow (a network's limit): spreading is what makes it fast.
    *srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(2);
    let e = env("spread");
    let m = manager(&e, vec![net("net-a", A, true), net("net-b", B, true)]).await;
    let dto = on(&m, &[]).await;
    assert!(dto.enabled && dto.blocked.is_none(), "{dto:?}");
    assert_eq!(
        dto.networks.iter().map(|n| (n.name.as_str(), n.main, n.chosen)).collect::<Vec<_>>(),
        [("net-a", true, true), ("net-b", false, true)]
    );

    let d = download(&m, &e, &srv, "/spread.bin", &data).await;
    let (a, b, normal) = shares(&srv);
    assert!(a > MB as u64 && b > MB as u64, "both networks carried parts: {a} / {b}");
    assert!(normal < 64 * 1024, "nothing but the server check went the normal route: {normal}");

    // The download's details say how much came through each network; Settings says each one worked.
    let diag = m.get_download_diagnostics(d.id).await.unwrap();
    let names: Vec<&str> = diag.networks.iter().map(|n| n.name.as_str()).collect();
    assert!(names.contains(&"net-a") && names.contains(&"net-b"), "{:?}", diag.networks);
    let total: u64 = diag.networks.iter().map(|n| n.bytes).sum();
    assert!(total >= data.len() as u64 && total <= data.len() as u64 + 256 * 1024, "{total}");
    assert!(diag.report.contains("Networks: net-a ") || diag.report.contains("Networks: net-b "), "{}", diag.report);
    assert!(!diag.report.contains("127.0.0.2") && !diag.report.contains("127.0.0.3"), "no network address in the report");
    let now = m.get_multi_network().await;
    assert!(now.networks.iter().all(|n| n.state == Some(LaneState::Working) && n.bytes > 0 && n.active == 0), "{now:?}");
}

#[tokio::test]
async fn a_network_that_fails_never_fails_a_download() {
    let data = random_data(6 * MB, 502);
    let srv = serve(data.clone()).await;
    let e = env("gone");
    let m = manager(&e, vec![net("good", A, true), net("gone", GONE, true)]).await;
    on(&m, &[]).await;
    download(&m, &e, &srv, "/gone.bin", &data).await;
    let dto = m.get_multi_network().await;
    let gone = dto.networks.iter().find(|n| n.name == "gone").unwrap();
    assert_eq!(gone.state, Some(LaneState::Resting), "the failed network rests: {dto:?}");
    assert!(served_to(&srv, A) > MB as u64, "the working network carried parts");
    // Its requests went through the normal route at once: no retry was spent on it.
    let d = m.list_downloads(Default::default()).await.unwrap();
    let diag = m.get_download_diagnostics(d[0].id).await.unwrap();
    assert_eq!(diag.retries, 0, "{:?}", diag.events);
}

#[tokio::test]
async fn a_server_that_refuses_a_network_gets_its_requests_the_normal_way() {
    let data = random_data(6 * MB, 503);
    let srv = serve(data.clone()).await;
    // A link signed for one address: the server refuses the other network.
    *srv.state.refuse_peer.lock().unwrap() = Some(IpAddr::V4(B));
    let e = env("refuse");
    let m = manager(&e, vec![net("net-a", A, true), net("net-b", B, true)]).await;
    on(&m, &[]).await;
    download(&m, &e, &srv, "/signed/tok/refused.bin", &data).await;
    assert_eq!(served_to(&srv, B), 0);
    assert!(served_to(&srv, A) > MB as u64);
    // The network itself is fine (another server may use it): it does not rest.
    let dto = m.get_multi_network().await;
    assert_ne!(dto.networks.iter().find(|n| n.name == "net-b").unwrap().state, Some(LaneState::Resting), "{dto:?}");
}

#[tokio::test]
async fn nothing_is_spread_when_off_with_one_network_through_a_proxy_or_another_route() {
    let data = random_data(3 * MB, 504);
    let srv = serve(data.clone()).await;
    let e = env("off");
    let m = manager(&e, vec![net("net-a", A, true), net("net-b", B, true)]).await;

    // Off (the default): the networks are listed, none is used.
    let dto = m.get_multi_network().await;
    assert!(!dto.enabled && dto.networks.len() == 2 && dto.networks.iter().all(|n| n.state.is_none()), "{dto:?}");
    download(&m, &e, &srv, "/off.bin", &data).await;
    assert_eq!((shares(&srv).0, shares(&srv).1), (0, 0));

    // One network left out: one is left, nothing to spread over.
    let dto = on(&m, &["net-b"]).await;
    assert!(!dto.networks.iter().find(|n| n.name == "net-b").unwrap().chosen);
    download(&m, &e, &srv, "/one.bin", &data).await;
    assert_eq!((shares(&srv).0, shares(&srv).1), (0, 0));

    // Saved: the next start has the same choice.
    drop(m);
    let m = manager(&e, vec![net("net-a", A, true), net("net-b", B, true)]).await;
    let dto = m.get_multi_network().await;
    assert!(dto.enabled && !dto.networks.iter().find(|n| n.name == "net-b").unwrap().chosen, "{dto:?}");

    // A proxy carries everything: not spread, and Settings says why.
    on(&m, &[]).await;
    let proxy = SetNetwork {
        proxy_mode: ProxyMode::Manual,
        proxy_url: Some("http://127.0.0.1:9".into()),
        proxy_username: None,
        proxy_password: None,
        user_agent: None,
    };
    m.set_network(proxy).await.unwrap();
    let dto = m.get_multi_network().await;
    assert_eq!(dto.blocked.as_deref(), Some("proxy"));
    assert!(dto.networks.iter().all(|n| n.state.is_none()), "{dto:?}");
    let direct = SetNetwork { proxy_mode: ProxyMode::None, proxy_url: None, proxy_username: None, proxy_password: None, user_agent: None };
    m.set_network(direct).await.unwrap();
    assert!(m.get_multi_network().await.blocked.is_none());
    drop(m);

    // The system reaches the server from an address none of the networks has (a VPN carries it): not spread.
    let e2 = env("vpn");
    let m = manager(&e2, vec![net("net-a", A, false), net("net-b", B, false)]).await;
    on(&m, &[]).await;
    let before = shares(&srv);
    download(&m, &e2, &srv, "/vpn.bin", &data).await;
    let after = shares(&srv);
    assert_eq!((after.0, after.1), (before.0, before.1), "nothing through the networks");
}

#[tokio::test]
async fn bad_choices_are_refused_and_change_nothing() {
    let e = env("bad");
    let m = manager(&e, vec![net("net-a", A, true), net("net-b", B, true)]).await;
    for skip in [vec!["a\nb".to_string()], vec![String::new()], vec!["x".repeat(300)], (0..65).map(|i| format!("n{i}")).collect()] {
        assert!(m.set_multi_network(SetMultiNetwork { enabled: true, skip }).await.is_err());
    }
    assert!(!m.get_multi_network().await.enabled);
    // Unknown fields are refused by the command itself.
    assert!(serde_json::from_str::<SetMultiNetwork>(r#"{"enabled":true,"skip":[],"bind":"10.0.0.1"}"#).is_err());
}

#[tokio::test]
async fn smart_throttle_is_held_off_while_several_networks_are_on() {
    let e = env("smart");
    let m = manager(&e, vec![net("net-a", A, true), net("net-b", B, true)]).await;
    m.set_smart_throttle(SetSmartThrottle { enabled: Some(true), ..Default::default() }).await.unwrap();
    assert!(m.smart_throttle_enabled());
    on(&m, &[]).await;
    let s = m.smart_throttle();
    assert!(s.held_by_networks && s.enabled && s.reason == "off", "held off, the user's switch kept: {s:?}");
    assert!(!m.smart_throttle_enabled(), "nothing slows the networks down");
    assert!(
        m.set_smart_throttle(SetSmartThrottle { enabled: Some(true), ..Default::default() }).await.is_err(),
        "cannot be turned on meanwhile"
    );
    // Saved: after a restart it is still held off.
    drop(m);
    let m = manager(&e, vec![net("net-a", A, true), net("net-b", B, true)]).await;
    assert!(m.smart_throttle().held_by_networks && !m.smart_throttle_enabled());
    // Turned off: the user's Smart throttle is back.
    m.set_multi_network(SetMultiNetwork { enabled: false, skip: vec![] }).await.unwrap();
    assert!(!m.smart_throttle().held_by_networks && m.smart_throttle_enabled());
}
