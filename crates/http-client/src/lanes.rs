//! Several networks at once: the ranged requests of a download are spread over the computer's networks (Wi-Fi and
//! a cable, a phone shared over USB...), each with its own connections bound to that network, so their speeds add up.
//!
//! * A **lane** is one network: a client whose connections leave through that network only (`SO_BINDTODEVICE` on
//!   Linux, `IP_BOUND_IF` on macOS, the network's own address on Windows, where the system then sends through it).
//! * A request goes to the lane with the least work for its speed (connections per measured byte rate, with a floor
//!   so a slow network still gets a share and is measured again).
//! * A lane that cannot connect, breaks a transfer, or presents another certificate, rests (30 s, then longer) and
//!   its request is sent again through the normal route at once: a network that stops working never fails a download.
//!   A server that refuses a lane (`401`, `403`, `404`, `410`, `451`: a link signed for one address, for example) is
//!   not asked through that lane again for a while; the same request goes through the normal route.
//! * Only for links that can be trusted to be the same file whatever the network: **https** (the certificate proves
//!   the server), to a **public** address, without a proxy. Everything else goes through the normal route, as before.
//!   Every answer is checked exactly like on the normal route (`Content-Range`, `If-Range`, the file's tag, the joins).
//! * Addresses of the networks are never logged: only their names.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use url::Url;

use crate::CachingResolver;

/// One network to send requests through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteSpec {
    /// The network's name, as the system calls it ("Wi-Fi", "enp5s0"...). Shown to the user; never an address.
    pub name: String,
    /// Bind to this interface (Linux, macOS). Ignored on Windows, which uses the addresses.
    pub interface: Option<String>,
    /// The network's own addresses: the source of its connections. A family without one is not used on it.
    pub v4: Option<Ipv4Addr>,
    pub v6: Option<Ipv6Addr>,
    /// Every address the computer has on this network (IPv6 has several): what the system's own route to a server
    /// must leave from for that server's requests to be spread (see [`Lanes::system_route_is_ours`]).
    pub addresses: Vec<IpAddr>,
}

/// What a lane is doing, for Settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum LaneState {
    /// Not used yet (no download needed it).
    Ready,
    /// Its last request worked.
    Working,
    /// It failed: it rests until it is tried again.
    Resting,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LaneStatus {
    pub name: String,
    pub active: usize,
    /// Bytes received through it since the app started.
    pub bytes: u64,
    pub state: LaneState,
}

/// First rest after a failure, doubled at each failure in a row, up to [`MAX_REST`].
pub const FIRST_REST: Duration = Duration::from_secs(5);
pub const MAX_REST: Duration = Duration::from_secs(5 * 60);
/// A transfer through a network that receives nothing for this long is given up (the network rests).
pub const LANE_STALL: Duration = Duration::from_secs(10);
/// How much waiting for bytes a transfer through one of several networks is judged over for crawling (see [`crawls`]).
pub const CRAWL_WINDOW: Duration = Duration::from_secs(4);

/// A transfer at `speed` crawls when the best network's connections go more than 8 times faster (`fastest`, bytes/s
/// per connection; nothing measured yet: never).
pub fn crawls(speed: f64, fastest: f64) -> bool {
    fastest > 0.0 && speed * 8.0 < fastest
}

/// How long a server's refusal of a lane keeps that server's requests off it.
pub const REFUSAL_REST: Duration = Duration::from_secs(10 * 60);
/// A lane's connection is never weighted below this speed (bytes/s), nor below a tenth of the fastest one's: it keeps
/// getting a connection now and then, which measures it again.
const MIN_RATE: f64 = 16.0 * 1024.0;

#[derive(Default)]
struct Health {
    down_until: Option<Instant>,
    strikes: u32,
    worked: bool,
    /// Bytes per second (smoothed), measured while it had connections.
    rate: f64,
    /// When it was last measured, its bytes then, and its connections then.
    measured: Option<(Instant, u64, usize)>,
    /// Servers (host names) that refused this lane, until when.
    refused: HashMap<String, Instant>,
}

pub(crate) struct Lane {
    pub(crate) spec: RouteSpec,
    /// The network's addresses now (they change: IPv6 temporary addresses); `spec.addresses` is what it started with.
    addresses: RwLock<Vec<IpAddr>>,
    pub(crate) client: reqwest::Client,
    active: AtomicUsize,
    bytes: AtomicU64,
    health: Mutex<Health>,
    /// The network went away (no longer listed): its transfers end at once, their parts continue on the others.
    gone: std::sync::atomic::AtomicBool,
    gone_signal: tokio::sync::Notify,
}

impl Lane {
    /// The network is no longer there: every transfer through it ends now.
    pub(crate) fn mark_gone(&self) {
        self.gone.store(true, Ordering::SeqCst);
        self.gone_signal.notify_waiters();
    }

    /// Wait until the network goes away.
    pub(crate) async fn until_gone(&self) {
        loop {
            let notified = self.gone_signal.notified();
            if self.gone.load(Ordering::SeqCst) {
                return;
            }
            notified.await;
        }
    }

    pub(crate) fn new(spec: RouteSpec, client: reqwest::Client) -> Self {
        let addresses = RwLock::new(spec.addresses.clone());
        Lane {
            spec,
            client,
            addresses,
            active: AtomicUsize::new(0),
            bytes: AtomicU64::new(0),
            health: Mutex::new(Health::default()),
            gone: std::sync::atomic::AtomicBool::new(false),
            gone_signal: tokio::sync::Notify::new(),
        }
    }

    /// Bound the same way (same network, interface, source addresses): the same lane.
    pub(crate) fn binds_like(&self, spec: &RouteSpec) -> bool {
        (&self.spec.name, &self.spec.interface, self.spec.v4, self.spec.v6) == (&spec.name, &spec.interface, spec.v4, spec.v6)
    }

    pub(crate) fn set_addresses(&self, a: Vec<IpAddr>) {
        *self.addresses.write().unwrap_or_else(|p| p.into_inner()) = a;
    }

    pub(crate) fn addresses(&self) -> Vec<IpAddr> {
        self.addresses.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    fn health(&self) -> std::sync::MutexGuard<'_, Health> {
        self.health.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn usable_for(&self, host: &str, now: Instant) -> bool {
        let h = self.health();
        h.down_until.is_none_or(|t| now >= t) && h.refused.get(host).is_none_or(|t| now >= *t)
    }

    /// The smoothed speed of one of its connections (bytes/s), measured again when at least a second passed with
    /// connections running. Per connection, not in total: a network given fewer connections would measure slower in
    /// total, get fewer still, and never show what it can do.
    fn rate(&self, now: Instant) -> f64 {
        let bytes = self.bytes.load(Ordering::Relaxed);
        let active = self.active.load(Ordering::Relaxed);
        let mut h = self.health();
        match h.measured {
            Some((at, before, was)) if active > 0 && now.duration_since(at) >= Duration::from_secs(1) => {
                let total = bytes.saturating_sub(before) as f64 / now.duration_since(at).as_secs_f64();
                let per = total / ((was + active) as f64 / 2.0).max(1.0);
                h.rate = if h.rate == 0.0 { per } else { 0.5 * h.rate + 0.5 * per };
                h.measured = Some((now, bytes, active));
            }
            // Idle: its speed is not "zero", it is unknown since; measuring starts again with its next connection.
            _ if active == 0 => h.measured = Some((now, bytes, 0)),
            None => h.measured = Some((now, bytes, active)),
            _ => {}
        }
        h.rate
    }

    pub(crate) fn worked(&self) {
        let mut h = self.health();
        h.worked = true;
        h.strikes = 0;
        h.down_until = None;
    }

    /// It failed: rest, longer at each failure in a row.
    /// Failures while it already rests (all its connections breaking at once when it is unplugged) are the same
    /// failure: only one counted, so a network that comes back is tried again soon, not after the longest rest.
    pub(crate) fn failed(&self, why: &dyn std::fmt::Display) {
        let mut h = self.health();
        if h.down_until.is_some_and(|t| Instant::now() < t) {
            return;
        }
        let rest = FIRST_REST.saturating_mul(1u32 << h.strikes.min(6)).min(MAX_REST);
        h.strikes = h.strikes.saturating_add(1);
        h.down_until = Some(Instant::now() + rest);
        tracing::info!(network = %self.spec.name, rest_secs = rest.as_secs(), error = %why, "a network failed: its requests use the others for now");
    }

    /// This server will not answer through this network (a link signed for another address...).
    pub(crate) fn refused_by(&self, host: &str, status: u16) {
        self.health().refused.insert(host.to_string(), Instant::now() + REFUSAL_REST);
        tracing::info!(network = %self.spec.name, status, "the server refuses this network: its requests use the normal route");
    }

    pub(crate) fn status(&self) -> LaneStatus {
        let now = Instant::now();
        let h = self.health();
        let state = if h.down_until.is_some_and(|t| now < t) {
            LaneState::Resting
        } else if h.worked {
            LaneState::Working
        } else {
            LaneState::Ready
        };
        LaneStatus {
            name: self.spec.name.clone(),
            active: self.active.load(Ordering::Relaxed),
            bytes: self.bytes.load(Ordering::Relaxed),
            state,
        }
    }
}

/// A request in flight on a lane: counted while it lives, its bytes added to the lane's.
pub(crate) struct LaneUse(pub(crate) Arc<Lane>);

impl LaneUse {
    pub(crate) fn new(lane: Arc<Lane>) -> Self {
        lane.active.fetch_add(1, Ordering::Relaxed);
        LaneUse(lane)
    }

    pub(crate) fn received(&self, n: usize) {
        self.0.bytes.fetch_add(n as u64, Ordering::Relaxed);
    }
}

impl Drop for LaneUse {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::Relaxed);
    }
}

/// How long the answer of [`Lanes::system_route_is_ours`] is kept for a server.
const ROUTE_CHECK_TTL: Duration = Duration::from_secs(60);

/// The lanes, shared by every copy of the client (a download already running sees networks come and go).
#[derive(Clone, Default)]
pub(crate) struct Lanes {
    list: Arc<RwLock<Vec<Arc<Lane>>>>,
    /// Servers whose system route was checked: when, and whether it leaves from one of the networks.
    checked: Arc<Mutex<HashMap<String, (Instant, bool)>>>,
}

impl Lanes {
    pub(crate) fn list(&self) -> Vec<Arc<Lane>> {
        self.list.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    pub(crate) fn set(&self, lanes: Vec<Arc<Lane>>) {
        *self.list.write().unwrap_or_else(|p| p.into_inner()) = lanes;
        self.checked.lock().unwrap_or_else(|p| p.into_inner()).clear();
    }

    /// Would the system itself send this server's requests over one of the networks? Not when a VPN or another
    /// tunnel carries them (the whole traffic, or only some addresses): binding them to a network would go around it.
    /// Asked of the system for every address of the server (no packet is sent), kept a minute.
    pub(crate) async fn system_route_is_ours(&self, host: &str, dns: &CachingResolver) -> bool {
        let now = Instant::now();
        if let Some((at, ok)) = self.checked.lock().unwrap_or_else(|p| p.into_inner()).get(host) {
            if now.duration_since(*at) < ROUTE_CHECK_TTL {
                return *ok;
            }
        }
        let ours: HashSet<IpAddr> = self.list().iter().flat_map(|l| l.addresses()).collect();
        let ok = match dns.resolve_host(host).await {
            Ok(addrs) => {
                let ips: Vec<IpAddr> = addrs.iter().map(|a| a.ip()).take(16).collect();
                // Addresses the system has no route to at all (IPv6 while no network has it) are skipped: nothing goes
                // there, through a VPN or not. Every reachable one must leave from one of the networks.
                let routes: Vec<IpAddr> = ips.iter().filter_map(|ip| system_source_for(*ip)).filter(|src| reaches_internet(*src)).collect();
                !routes.is_empty() && routes.iter().all(|src| ours.contains(src))
            }
            Err(_) => false,
        };
        if !ok {
            tracing::info!("the system sends this server's requests another way (a VPN...): they are not spread");
        }
        self.checked.lock().unwrap_or_else(|p| p.into_inner()).insert(host.to_string(), (now, ok));
        ok
    }

    /// The best speed per connection measured on any network (bytes/s; 0: none measured yet).
    pub(crate) fn fastest_rate(&self) -> f64 {
        self.list().iter().map(|l| l.health().rate).fold(0.0, f64::max)
    }

    /// The lane for the next request to `host`: `None` = the normal route (fewer than two networks, or none usable).
    pub(crate) fn pick(&self, host: &str) -> Option<Arc<Lane>> {
        let lanes = self.list();
        if lanes.len() < 2 {
            return None;
        }
        let now = Instant::now();
        let usable: Vec<(Arc<Lane>, f64)> = lanes.iter().filter(|l| l.usable_for(host, now)).map(|l| (l.clone(), l.rate(now))).collect();
        let fastest = usable.iter().map(|(_, r)| *r).fold(0.0, f64::max);
        let floor = (fastest / 10.0).max(MIN_RATE);
        // A network not measured yet is counted as fast as the fastest: it gets its share at once and is measured
        // (counted as slow, it would never be tried).
        let usable: Vec<(Arc<Lane>, f64)> =
            usable.into_iter().map(|(l, r)| if r == 0.0 { (l, fastest.max(MIN_RATE)) } else { (l, r) }).collect();
        usable
            .into_iter()
            .map(|(l, r)| {
                let load = (l.active.load(Ordering::Relaxed) + 1) as f64 / r.max(floor);
                (l, load)
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(l, _)| l)
    }
}

/// The address the system would send from to reach `ip`: the route it picks. A datagram socket "connected" to it
/// sends nothing: the system only chooses the route and the source address. It is never bound by hand, never sent
/// on or read, and closed at once (`tests/security/policy.test.mjs` holds it to that).
pub fn system_source_for(ip: IpAddr) -> Option<IpAddr> {
    use socket2::{Domain, Protocol, Socket, Type};
    let to = SocketAddr::new(ip, 443);
    let s = Socket::new(Domain::for_address(to), Type::DGRAM, Some(Protocol::UDP)).ok()?;
    s.connect(&to.into()).ok()?;
    s.local_addr().ok()?.as_socket().map(|a| a.ip())
}

/// A source address the internet can answer: not link-local, loopback or unspecified (a router announcing an IPv6
/// route to a computer without an IPv6 address of its own leaves only `fe80::...`: nothing gets through there).
pub(crate) fn reaches_internet(src: IpAddr) -> bool {
    match src {
        IpAddr::V4(a) => !(a.is_link_local() || a.is_unspecified()),
        IpAddr::V6(a) => !(a.is_unspecified() || (a.segments()[0] & 0xffc0) == 0xfe80),
    }
}

/// May this link's requests be spread over several networks? Only https (the certificate proves it is the same
/// server whatever the network), to a public address, and never through a proxy.
pub fn spreadable(url: &Url, through_proxy: bool) -> bool {
    url.scheme() == "https" && !through_proxy && !crate::is_local_host(url)
}

/// The resolver of a lane: the client's names cache, keeping only the address families the network has (a
/// connection to the other family would not leave through it).
#[derive(Clone)]
pub(crate) struct FamilyResolver {
    pub(crate) inner: CachingResolver,
    pub(crate) v4: bool,
    pub(crate) v6: bool,
}

impl reqwest::dns::Resolve for FamilyResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let this = self.clone();
        Box::pin(async move {
            let addrs: Vec<SocketAddr> = this
                .inner
                .resolve_host(name.as_str())
                .await?
                .into_iter()
                .filter(|a| match a.ip() {
                    IpAddr::V4(_) => this.v4,
                    IpAddr::V6(_) => this.v6,
                })
                .collect();
            if addrs.is_empty() {
                return Err(Box::new(std::io::Error::new(std::io::ErrorKind::NotFound, "no address of this family")) as _);
            }
            Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

/// Connections bound to a network by its interface (Linux, macOS and their kin); elsewhere (Windows) by its address.
pub(crate) const BIND_BY_INTERFACE: bool = cfg!(any(
    target_os = "android",
    target_os = "fuchsia",
    target_os = "illumos",
    target_os = "ios",
    target_os = "linux",
    target_os = "macos",
    target_os = "solaris",
    target_os = "tvos",
    target_os = "visionos",
    target_os = "watchos",
));

/// How a lane's connections are bound: the source address, and the families it can reach servers on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Binding {
    pub(crate) local: Option<IpAddr>,
    pub(crate) v4: bool,
    pub(crate) v6: bool,
}

/// `None`: nothing to bind it to on this system (no interface binding, and no address).
pub(crate) fn binding(spec: &RouteSpec) -> Option<Binding> {
    // One source address per client: its own IPv4 one if it has one, else its IPv6 one.
    let local = spec.v4.map(IpAddr::V4).or(spec.v6.map(IpAddr::V6));
    if BIND_BY_INTERFACE && spec.interface.as_deref().is_some_and(|i| !i.is_empty()) {
        // The interface carries both families; with addresses given, only the ones it has.
        let (v4, v6) = match (spec.v4, spec.v6) {
            (None, None) => (true, true),
            (a, b) => (a.is_some(), b.is_some()),
        };
        Some(Binding { local, v4, v6 })
    } else {
        // By address only: a connection of the other family would not be bound, so it is not made.
        let ip = local?;
        Some(Binding { local: Some(ip), v4: ip.is_ipv4(), v6: ip.is_ipv6() })
    }
}

#[cfg(any(
    target_os = "android",
    target_os = "fuchsia",
    target_os = "illumos",
    target_os = "ios",
    target_os = "linux",
    target_os = "macos",
    target_os = "solaris",
    target_os = "tvos",
    target_os = "visionos",
    target_os = "watchos",
))]
pub(crate) fn bind_interface(b: reqwest::ClientBuilder, spec: &RouteSpec) -> reqwest::ClientBuilder {
    match spec.interface.as_deref().filter(|i| !i.is_empty()) {
        Some(i) => b.interface(i),
        None => b,
    }
}

#[cfg(not(any(
    target_os = "android",
    target_os = "fuchsia",
    target_os = "illumos",
    target_os = "ios",
    target_os = "linux",
    target_os = "macos",
    target_os = "solaris",
    target_os = "tvos",
    target_os = "visionos",
    target_os = "watchos",
)))]
pub(crate) fn bind_interface(b: reqwest::ClientBuilder, _spec: &RouteSpec) -> reqwest::ClientBuilder {
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lane(name: &str) -> Arc<Lane> {
        let spec = RouteSpec { name: name.into(), interface: None, v4: None, v6: None, addresses: vec![] };
        Arc::new(Lane::new(spec, reqwest::Client::new()))
    }

    #[test]
    fn only_https_to_public_addresses_without_a_proxy_is_spread() {
        let u = |s: &str| Url::parse(s).unwrap();
        assert!(spreadable(&u("https://cdn.example.com/f.iso"), false));
        assert!(!spreadable(&u("https://cdn.example.com/f.iso"), true), "never through a proxy");
        assert!(!spreadable(&u("http://cdn.example.com/f.iso"), false), "plain http: another network could change it");
        assert!(!spreadable(&u("https://192.168.1.10/f.iso"), false), "a local address is reachable from one network");
        assert!(!spreadable(&u("https://nas.local/f.iso"), false));
    }

    #[test]
    fn one_network_is_not_spread_and_requests_go_to_the_least_busy() {
        let lanes = Lanes::default();
        lanes.set(vec![lane("a")]);
        assert!(lanes.pick("h").is_none(), "one network: the normal route");
        lanes.set(vec![lane("a"), lane("b")]);
        let first = LaneUse::new(lanes.pick("h").unwrap());
        let second = LaneUse::new(lanes.pick("h").unwrap());
        assert_ne!(first.0.spec.name, second.0.spec.name, "the second request goes to the other network");
        drop(first);
        let third = lanes.pick("h").unwrap();
        assert_ne!(third.spec.name, second.0.spec.name);
    }

    #[test]
    fn a_network_not_measured_yet_gets_its_share_at_once() {
        // The usual network is measured and fast; two new ones are not measured: they are tried, not left idle.
        let lanes = Lanes::default();
        let (cable, phone, wifi) = (lane("cable"), lane("phone"), lane("wifi"));
        lanes.set(vec![cable.clone(), phone, wifi]);
        cable.health().rate = 1_500_000.0;
        cable.health().measured = Some((Instant::now(), 0, 0));
        let held: Vec<_> = (0..3).map(|_| LaneUse::new(lanes.pick("h").unwrap())).collect();
        let mut names: Vec<_> = held.iter().map(|u| u.0.spec.name.clone()).collect();
        names.sort();
        assert_eq!(names, ["cable", "phone", "wifi"], "one connection on each");
    }

    #[test]
    fn a_failed_network_rests_and_a_refusal_is_per_server() {
        let lanes = Lanes::default();
        let (a, b) = (lane("a"), lane("b"));
        lanes.set(vec![a.clone(), b.clone()]);
        a.failed(&"connection refused");
        for _ in 0..5 {
            assert_eq!(lanes.pick("h").unwrap().spec.name, "b");
        }
        assert_eq!(a.status().state, LaneState::Resting);
        b.failed(&"x");
        assert!(lanes.pick("h").is_none(), "no usable network: the normal route");
        // Rests grow with failures in a row, never beyond the longest.
        let c = lane("c");
        // All its connections failing at once: one failure, the first rest.
        for _ in 0..10 {
            c.failed(&"x");
        }
        let left = c.health().down_until.unwrap() - Instant::now();
        assert!(left <= FIRST_REST && left > FIRST_REST - Duration::from_secs(5), "{left:?}");
        // Failing again each time its rest is over: longer rests.
        for _ in 0..10 {
            c.health().down_until = Some(Instant::now() - Duration::from_secs(1));
            c.failed(&"x");
        }
        let left = c.health().down_until.unwrap() - Instant::now();
        assert!(left <= MAX_REST && left > MAX_REST - Duration::from_secs(5));
        c.worked();
        assert_eq!(c.status().state, LaneState::Working);

        let lanes = Lanes::default();
        let (a, b) = (lane("a"), lane("b"));
        lanes.set(vec![a.clone(), b]);
        a.refused_by("signed.example", 403);
        for _ in 0..5 {
            let _keep = LaneUse::new(lanes.pick("signed.example").unwrap());
            assert_eq!(_keep.0.spec.name, "b");
        }
        let picks: Vec<_> = (0..4).map(|_| LaneUse::new(lanes.pick("other.example").unwrap())).collect();
        assert!(picks.iter().any(|p| p.0.spec.name == "a"), "another server still uses it");
    }

    #[test]
    fn a_faster_network_gets_more_requests_and_a_slow_one_still_some() {
        let lanes = Lanes::default();
        let (fast, slow) = (lane("fast"), lane("slow"));
        lanes.set(vec![fast.clone(), slow.clone()]);
        let t0 = Instant::now() - Duration::from_secs(2);
        for (l, rate) in [(&fast, 10_000_000.0), (&slow, 1_000_000.0)] {
            l.health().rate = rate;
            l.health().measured = Some((t0, 0, 0));
        }
        let held: Vec<_> = (0..11).map(|_| LaneUse::new(lanes.pick("h").unwrap())).collect();
        let on_slow = held.iter().filter(|u| u.0.spec.name == "slow").count();
        assert!((1..=2).contains(&on_slow), "about a tenth on the slow one, got {on_slow} of 11");
    }

    #[tokio::test]
    async fn a_server_the_system_reaches_another_way_is_never_spread() {
        // The system reaches 127.0.0.1 from 127.0.0.1: "ours" only when a network has that address.
        let dns = CachingResolver::system(Duration::from_secs(60));
        assert_eq!(system_source_for(IpAddr::V4(Ipv4Addr::LOCALHOST)), Some(IpAddr::V4(Ipv4Addr::LOCALHOST)));
        let with = |addresses: Vec<IpAddr>| {
            let lanes = Lanes::default();
            let mk = |n: &str| {
                let spec = RouteSpec { name: n.into(), interface: None, v4: None, v6: None, addresses: addresses.clone() };
                Arc::new(Lane::new(spec, reqwest::Client::new()))
            };
            lanes.set(vec![mk("a"), mk("b")]);
            lanes
        };
        assert!(with(vec![IpAddr::V4(Ipv4Addr::LOCALHOST)]).system_route_is_ours("127.0.0.1", &dns).await);
        // A tunnel's address (not one of the networks'): its servers keep the system's route.
        let tunnel = with(vec![IpAddr::V4(Ipv4Addr::new(10, 8, 0, 2))]);
        assert!(!tunnel.system_route_is_ours("127.0.0.1", &dns).await);
        assert!(!tunnel.system_route_is_ours("name.invalid", &dns).await, "an unknown name is not spread");
        // Kept a minute, forgotten when the networks change.
        tunnel.checked.lock().unwrap().insert("127.0.0.1".into(), (Instant::now(), true));
        assert!(tunnel.system_route_is_ours("127.0.0.1", &dns).await);
        tunnel.set(tunnel.list());
        assert!(!tunnel.system_route_is_ours("127.0.0.1", &dns).await);
        // A server that also has an address the system has no route to (IPv6 while no network has it): judged by
        // the reachable ones.
        let dns_both = CachingResolver::with_lookup(Duration::from_secs(60), |_| {
            Box::pin(async { Ok(vec!["127.0.0.1:0".parse().unwrap(), "[2001:db8:dead::1]:0".parse().unwrap()]) })
        });
        if system_source_for("2001:db8:dead::1".parse().unwrap()).is_none() {
            assert!(with(vec![IpAddr::V4(Ipv4Addr::LOCALHOST)]).system_route_is_ours("both.example", &dns_both).await);
        }
    }

    #[tokio::test]
    async fn a_network_that_goes_away_ends_its_transfers_at_once() {
        let l = lane("phone");
        let waiting = {
            let l = l.clone();
            tokio::spawn(async move { l.until_gone().await })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!waiting.is_finished());
        l.mark_gone();
        tokio::time::timeout(Duration::from_secs(1), waiting).await.expect("ends at once").unwrap();
        // Already gone: a new wait ends at once too.
        tokio::time::timeout(Duration::from_secs(1), l.until_gone()).await.unwrap();
    }

    #[test]
    fn a_route_from_a_link_local_address_reaches_nothing() {
        assert!(!reaches_internet("fe80::900d:1e2c:90ec:6138".parse().unwrap()));
        assert!(!reaches_internet("169.254.3.4".parse().unwrap()));
        assert!(!reaches_internet("::".parse().unwrap()));
        assert!(reaches_internet("192.168.1.67".parse().unwrap()));
        assert!(reaches_internet("2c0f:f290::1".parse().unwrap()));
        assert!(reaches_internet("127.0.0.1".parse().unwrap()), "the tests' own server");
    }

    #[test]
    fn a_crawling_transfer_is_told_from_a_slower_network() {
        assert!(crawls(30_000.0, 300_000.0), "a tenth of the best: crawls");
        assert!(!crawls(100_000.0, 300_000.0), "a third (a slower Wi-Fi): fine");
        assert!(!crawls(1.0, 0.0), "nothing measured yet: never");
        let lanes = Lanes::default();
        let (a, b) = (lane("a"), lane("b"));
        lanes.set(vec![a.clone(), b.clone()]);
        assert_eq!(lanes.fastest_rate(), 0.0);
        a.health().rate = 250_000.0;
        b.health().rate = 90_000.0;
        assert_eq!(lanes.fastest_rate(), 250_000.0);
    }

    #[test]
    fn a_network_with_only_one_family_keeps_to_it() {
        let s = |iface: bool, v4: bool, v6: bool| RouteSpec {
            name: "n".into(),
            interface: iface.then(|| "n".into()),
            v4: v4.then_some(Ipv4Addr::new(10, 0, 0, 2)),
            v6: v6.then_some(Ipv6Addr::LOCALHOST),
            addresses: vec![],
        };
        let fam = |b: Option<Binding>| b.map(|b| (b.v4, b.v6));
        // By address (Windows, or no interface name): one source address, its family only.
        assert_eq!(fam(binding(&s(false, true, true))), Some((true, false)));
        assert_eq!(fam(binding(&s(false, false, true))), Some((false, true)));
        assert_eq!(binding(&s(false, false, false)), None, "nothing to bind to");
        if BIND_BY_INTERFACE {
            assert_eq!(fam(binding(&s(true, true, false))), Some((true, false)));
            assert_eq!(fam(binding(&s(true, true, true))), Some((true, true)));
            assert_eq!(fam(binding(&s(true, false, false))), Some((true, true)), "bound by interface only: both");
        } else {
            assert_eq!(binding(&s(true, false, false)), None, "no interface binding here, and no address");
        }
    }
}
