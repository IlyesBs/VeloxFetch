//! Several networks at once (Settings > Network): a download's connections are spread over the computer's networks
//! (Wi-Fi and a cable, a phone shared over USB...), so their speeds add up. Off until the user turns it on; each
//! network can be left out (a phone's data allowance...).
//!
//! * The networks are read from the system (`sysinfo`): adapters with a hardware address, up, with an address that can
//!   reach the internet (not a link-local one), and not a virtual adapter (containers, virtual machines, tunnels).
//!   Read again every [`REFRESH`] while it is on: a network that comes is used at once, one that goes is left (its
//!   transfers end at once). Each is named by its kind (`system_integration::adapters`).
//! * Never around a VPN: per server, `http_client` checks the system's own route to it first (a VPN carrying
//!   everything, or only some addresses, keeps that server's traffic). When the system's route to the internet does
//!   not leave from one of these networks, Settings says a VPN carries the traffic.
//! * Smart throttle is held off while it is on (every network at full speed).
//! * Never through a proxy (Settings > Network, or the system's in the environment): the proxy carries everything.
//! * The spreading itself, its checks and its fallbacks are in `http_client::lanes`.
//!
//! Only network names are shown and logged, never their addresses.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use http_client::RouteSpec;
use sysinfo::{InterfaceOperationalState, Networks};
use system_integration::AdapterKind;

use super::*;

const ON_KEY: &str = "network.multi";
const SKIP_KEY: &str = "network.multi_skip";
/// How often the networks are read again while it is on.
pub(super) const REFRESH: Duration = Duration::from_secs(5);
/// A reading of the system's networks that takes longer is not waited for: the last one is kept (a network driver
/// that hangs in the kernel can block every network call of the system).
const READ_LIMIT: Duration = Duration::from_secs(2);
/// Networks the user can leave out, at most (and the length of a name).
const MAX_SKIP: usize = 64;
const MAX_NAME: usize = 256;

/// A network found on this computer.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Found {
    pub spec: RouteSpec,
    /// The system's own route to the internet leaves from it.
    pub main: bool,
    /// What it is (Wi-Fi, a phone...): the name the user sees.
    pub kind: AdapterKind,
}

#[derive(Default)]
pub(super) struct MultiNet {
    pub on: bool,
    /// Names the user left out.
    pub skip: Vec<String>,
    pub found: Vec<Found>,
    /// What each adapter is, by name, once read (the system is asked only about new names).
    pub kinds: HashMap<String, AdapterKind>,
    /// A reading of the system is under way (one at a time: a system whose network calls hang never piles them up).
    pub reading: Arc<std::sync::atomic::AtomicBool>,
    /// The system's route to the internet goes another way (a VPN...), in two readings in a row.
    pub tunnel: bool,
    /// The last reading said so.
    pub tunnel_seen: bool,
    pub read_at: Option<Instant>,
}

impl MultiNet {
    pub(super) fn load(store: &SqliteStore) -> Self {
        let get = |k: &str| store.get_setting(k).ok().flatten();
        let skip = get(SKIP_KEY)
            .and_then(|v| serde_json::from_str::<Vec<String>>(&v).ok())
            .map(|v| v.into_iter().filter(|n| valid_name(n)).take(MAX_SKIP).collect())
            .unwrap_or_default();
        MultiNet { on: get(ON_KEY).as_deref() == Some("1"), skip, ..Default::default() }
    }
}

fn valid_name(n: &str) -> bool {
    !n.is_empty() && n.len() <= MAX_NAME && !n.chars().any(char::is_control)
}

/// Names of virtual adapters (containers, virtual machines, tunnels, packet capture), in lower case: never a network
/// of their own to the internet.
const VIRTUAL: [&str; 21] = [
    "docker",
    "veth",
    "br-",
    "virbr",
    "vmnet",
    "vboxnet",
    "virtualbox",
    "vmware",
    "hyper-v",
    "vethernet",
    "loopback",
    "tailscale",
    "zerotier",
    "wireguard",
    "nordlynx",
    "openvpn",
    "vpn",
    "npcap",
    "teredo",
    "isatap",
    "tap-",
];
const VIRTUAL_PREFIX: [&str; 6] = ["tun", "tap", "wg", "utun", "ipsec", "cni"];

pub(super) fn is_virtual(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    // `lo`, `lo0`: the computer itself.
    let loopback = n.strip_prefix("lo").is_some_and(|rest| rest.chars().all(|c| c.is_ascii_digit()));
    loopback || VIRTUAL.iter().any(|v| n.contains(v)) || VIRTUAL_PREFIX.iter().any(|p| n.starts_with(p))
}

/// An address that can reach the internet: not loopback, link-local or unspecified; for IPv6, a global one.
pub(super) fn usable(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(a) => !(a.is_loopback() || a.is_link_local() || a.is_unspecified() || a.is_broadcast() || a.is_multicast()),
        IpAddr::V6(a) => (a.segments()[0] & 0xe000) == 0x2000,
    }
}

/// The address the system's own route to the internet leaves from, for each family (`None`: no such route).
fn system_sources() -> (Option<IpAddr>, Option<IpAddr>) {
    // Documentation addresses: never reached (nothing is sent), the route to them is the default one.
    (
        http_client::system_source_for(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1))),
        http_client::system_source_for(IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1))),
    )
}

/// The computer's networks now, and whether its route to the internet goes another way. Reads the system: call it
/// off the async threads.
pub(super) fn discover(known: HashMap<String, AdapterKind>) -> (Vec<Found>, bool, HashMap<String, AdapterKind>) {
    let nets = Networks::new_with_refreshed_list();
    let mut list: Vec<(String, Vec<IpAddr>)> = Vec::new();
    let mut macs = std::collections::HashSet::new();
    let mut names: Vec<&String> = nets.list().keys().collect();
    names.sort();
    for name in names {
        let n = &nets.list()[name];
        let mac = n.mac_address();
        let down = matches!(
            n.operational_state(),
            InterfaceOperationalState::Down
                | InterfaceOperationalState::LowerLayerDown
                | InterfaceOperationalState::NotPresent
                | InterfaceOperationalState::Dormant
                | InterfaceOperationalState::Testing
        );
        if mac.is_unspecified() || down || is_virtual(name) || !valid_name(name) {
            continue;
        }
        let mut addrs: Vec<IpAddr> = n.ip_networks().iter().map(|i| i.addr).filter(|a| usable(*a)).collect();
        // The system lists them in no fixed order: sorted, a network read again compares equal.
        addrs.sort();
        addrs.dedup();
        // One adapter seen twice (a capture filter mirroring it) counts once.
        if addrs.is_empty() || !macs.insert(mac.0) {
            continue;
        }
        list.push((name.clone(), addrs));
    }
    let (src4, src6) = system_sources();
    let names: Vec<String> = list.iter().map(|(n, _)| n.clone()).collect();
    let new: Vec<String> = names.iter().filter(|n| !known.contains_key(*n)).cloned().collect();
    let mut kinds = known;
    if !new.is_empty() {
        kinds.extend(system_integration::adapters::adapter_kinds(&new));
    }
    let (mut found, tunnel) = shape(list, src4, src6);
    for f in &mut found {
        f.kind = kinds.get(&f.spec.name).copied().unwrap_or(AdapterKind::Other);
    }
    // Only the adapters there now are remembered (a name reused by another device is asked again).
    kinds.retain(|n, _| names.contains(n));
    (found, tunnel, kinds)
}

/// Run `read` on a blocking thread, one at a time (`reading`), waiting at most `limit`. `None`: one is still on its way
/// (or stuck), or this one took too long; it goes on by itself and clears `reading` when it ends, so a system whose
/// network calls hang never piles threads up.
pub(super) async fn read_bounded<T: Send + 'static>(
    reading: &Arc<std::sync::atomic::AtomicBool>,
    limit: Duration,
    read: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    if reading.swap(true, Ordering::SeqCst) {
        return None;
    }
    let flag = reading.clone();
    let task = tokio::task::spawn_blocking(move || {
        let r = read();
        flag.store(false, Ordering::SeqCst);
        r
    });
    match tokio::time::timeout(limit, task).await {
        Ok(Ok(r)) => Some(r),
        Ok(Err(_)) => {
            reading.store(false, Ordering::SeqCst);
            None
        }
        Err(_) => {
            tracing::warn!("reading the networks takes too long: the last reading is kept");
            None
        }
    }
}

/// [`discover`] from what the system said: each network's spec, which one the system's route leaves from, and whether
/// that route leaves from none of them (a VPN or another tunnel).
pub(super) fn shape(list: Vec<(String, Vec<IpAddr>)>, src4: Option<IpAddr>, src6: Option<IpAddr>) -> (Vec<Found>, bool) {
    let found: Vec<Found> = list
        .into_iter()
        .map(|(name, addresses)| {
            let v4 = addresses.iter().find_map(|a| match a {
                IpAddr::V4(v) => Some(*v),
                _ => None,
            });
            let v6 = addresses.iter().find_map(|a| match a {
                IpAddr::V6(v) => Some(*v),
                _ => None,
            });
            // The usual network: the one the system's IPv4 route leaves from (its IPv6 one when it has no IPv4 route; a
            // phone often carries IPv6 alongside a cable that carries IPv4).
            let main = src4.or(src6).is_some_and(|s| addresses.contains(&s));
            Found { spec: RouteSpec { interface: Some(name.clone()), name, v4, v6, addresses }, main, kind: AdapterKind::Other }
        })
        .collect();
    // The system's route to the internet: its IPv4 one (its IPv6 one only without IPv4). Offline (no route at all) is
    // not a tunnel; a route from an address none of the networks has is (a network going away leaves its IPv6 route
    // for a moment: IPv6 alone never decides while there is IPv4).
    let tunnel = src4.or(src6).is_some_and(|s| !found.iter().any(|f| f.spec.addresses.contains(&s)));
    (found, tunnel)
}

impl DownloadManager {
    fn multinet(&self) -> std::sync::MutexGuard<'_, MultiNet> {
        self.inner.multinet.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Why nothing is spread although it is on: `vpn`, `proxy`.
    fn multinet_blocked(&self, m: &MultiNet) -> Option<&'static str> {
        let proxy = match self.network().mode {
            ProxyMode::Manual => true,
            ProxyMode::System => http_client::system_proxy_in_environment(),
            ProxyMode::None => false,
        };
        if proxy {
            Some("proxy")
        } else if m.tunnel {
            Some("vpn")
        } else {
            None
        }
    }

    /// Read the networks again (when on and the last reading is old enough, or `force`), and spread over the chosen
    /// ones.
    pub(super) async fn refresh_networks(&self, force: bool) {
        let due = {
            let m = self.multinet();
            force || (m.on && m.read_at.is_none_or(|t| t.elapsed() >= REFRESH))
        };
        if !due {
            return;
        }
        let read = match self.inner.cfg.test_networks.clone() {
            Some(list) => (
                list.into_iter().enumerate().map(|(i, spec)| Found { spec, main: i == 0, kind: AdapterKind::Other }).collect(),
                false,
                HashMap::new(),
            ),
            None => {
                let (known, reading) = {
                    let m = self.multinet();
                    (m.kinds.clone(), m.reading.clone())
                };
                match read_bounded(&reading, READ_LIMIT, move || discover(known)).await {
                    Some(r) => r,
                    None => return, // the last reading is kept
                }
            }
        };
        let want = {
            let mut m = self.multinet();
            let tunnel_now;
            (m.found, tunnel_now, m.kinds) = read;
            // Said in Settings only when two readings in a row agree (a network going away changes the routes for a
            // moment).
            m.tunnel = tunnel_now && m.tunnel_seen;
            m.tunnel_seen = tunnel_now;
            m.read_at = Some(Instant::now());
            // A VPN does not clear the networks: each server's own route is checked before its requests are spread
            // (`Lanes::system_route_is_ours`), which keeps a VPN's traffic on it, and a network going or coming back
            // (its addresses changing for a moment) never stops spreading for the others. A proxy carries everything.
            match (m.on, self.multinet_blocked(&m)) {
                (true, None | Some("vpn")) => m.found.iter().filter(|f| !m.skip.contains(&f.spec.name)).map(|f| f.spec.clone()).collect(),
                _ => Vec::new(),
            }
        };
        let now: Vec<RouteSpec> = self.inner.downloader.route_specs();
        if now != want {
            let names: Vec<&str> = want.iter().map(|s| s.name.as_str()).collect();
            tracing::info!(networks = ?names, "networks used for downloads");
            self.inner.downloader.set_routes(&want);
        }
    }

    /// What each network found is, by name.
    pub(super) fn network_kinds(&self) -> HashMap<String, AdapterKind> {
        self.multinet().found.iter().map(|f| (f.spec.name.clone(), f.kind)).collect()
    }

    pub async fn get_multi_network(&self) -> MultiNetworkDto {
        self.refresh_networks(true).await;
        self.multi_network_dto()
    }

    fn multi_network_dto(&self) -> MultiNetworkDto {
        let lanes = self.inner.downloader.routes();
        let m = self.multinet();
        MultiNetworkDto {
            enabled: m.on,
            blocked: if m.on { self.multinet_blocked(&m).map(str::to_string) } else { None },
            networks: m
                .found
                .iter()
                .map(|f| {
                    let lane = lanes.iter().find(|l| l.name == f.spec.name);
                    NetworkLaneDto {
                        name: f.spec.name.clone(),
                        kind: f.kind,
                        main: f.main,
                        chosen: !m.skip.contains(&f.spec.name),
                        state: lane.map(|l| l.state),
                        active: lane.map_or(0, |l| l.active),
                        bytes: lane.map_or(0, |l| l.bytes),
                    }
                })
                .collect(),
        }
    }

    /// Turn it on or off, and choose the networks left out. Saved; in force at once (downloads already running too).
    pub async fn set_multi_network(&self, req: SetMultiNetwork) -> Result<MultiNetworkDto, ManagerError> {
        if req.skip.len() > MAX_SKIP || req.skip.iter().any(|n| !valid_name(n)) {
            return Err(ManagerError::invalid("these network names cannot be saved"));
        }
        let mut skip = req.skip;
        skip.sort();
        skip.dedup();
        let values =
            [(ON_KEY, if req.enabled { "1" } else { "0" }.to_string()), (SKIP_KEY, serde_json::to_string(&skip).unwrap_or_default())];
        let store = self.inner.store.clone();
        blocking(move || {
            for (k, v) in values {
                store.set_setting(k, &v)?;
            }
            Ok(())
        })
        .await?;
        {
            let mut m = self.multinet();
            m.on = req.enabled;
            m.skip = skip;
        }
        // Every network at full speed: Smart throttle is held off while it is on.
        self.hold_smart_for_networks(req.enabled);
        tracing::info!(enabled = req.enabled, "several networks at once changed");
        Ok(self.get_multi_network().await)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v4(a: u8, b: u8, c: u8, d: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(a, b, c, d))
    }

    #[test]
    fn virtual_adapters_and_unusable_addresses_are_left_out() {
        for n in [
            "docker0",
            "veth12ab",
            "br-3f2a",
            "virbr0",
            "vmnet8",
            "vboxnet0",
            "VirtualBox Host-Only Network",
            "vEthernet (WSL)",
            "tun0",
            "tap0",
            "wg0",
            "utun3",
            "lo",
            "Loopback Pseudo-Interface 1",
            "tailscale0",
            "ZeroTier One",
            "NordLynx",
            "OpenVPN Data Channel Offload",
            "TAP-Windows Adapter V9",
            "Npcap Loopback Adapter",
            "ProtonVPN",
            "lo0",
            "cni0",
        ] {
            assert!(is_virtual(n), "{n}");
        }
        for n in [
            "Wi-Fi",
            "Ethernet",
            "Ethernet 2",
            "enp5s0",
            "wlp0s20f3",
            "en0",
            "en7",
            "eth0",
            "wlan0",
            "usb0",
            "Cellular",
            "Local Area Connection",
            "Local Area Connection* 2",
            "Bluetooth Network Connection",
        ] {
            assert!(!is_virtual(n), "{n}");
        }
        assert!(usable(v4(192, 168, 1, 67)) && usable(v4(10, 0, 0, 2)) && usable(v4(100, 70, 1, 1)));
        assert!(!usable(v4(127, 0, 0, 1)) && !usable(v4(169, 254, 3, 4)) && !usable(v4(0, 0, 0, 0)));
        assert!(usable("2a01:e0a::1".parse().unwrap()));
        assert!(!usable("fe80::1".parse().unwrap()) && !usable("::1".parse().unwrap()) && !usable("fd00::1".parse().unwrap()));
    }

    #[test]
    fn the_main_network_and_a_tunnel_are_told_apart() {
        let wifi = ("Wi-Fi".to_string(), vec![v4(192, 168, 1, 20), "2a01:e0a::20".parse().unwrap()]);
        let cable = ("Ethernet".to_string(), vec![v4(10, 0, 0, 5)]);
        let (found, tunnel) = shape(vec![wifi.clone(), cable.clone()], Some(v4(10, 0, 0, 5)), None);
        assert!(!tunnel);
        assert_eq!(found.iter().map(|f| (f.spec.name.as_str(), f.main)).collect::<Vec<_>>(), [("Wi-Fi", false), ("Ethernet", true)]);
        assert_eq!(found[0].spec.v4, Some(Ipv4Addr::new(192, 168, 1, 20)));
        assert!(found[0].spec.v6.is_some() && found[1].spec.v6.is_none());
        assert_eq!(found[0].spec.interface.as_deref(), Some("Wi-Fi"));
        // The system's route leaves from an address no network has: a VPN carries everything.
        assert!(shape(vec![wifi.clone(), cable.clone()], Some(v4(10, 8, 0, 2)), None).1);
        // IPv4 through a network, IPv6 from an address none has (a network going away): not a tunnel; IPv6 alone is.
        assert!(!shape(vec![wifi.clone(), cable.clone()], Some(v4(10, 0, 0, 5)), Some("2001:db8:9::2".parse().unwrap())).1);
        assert!(shape(vec![wifi.clone(), cable.clone()], None, Some("2001:db8:9::2".parse().unwrap())).1);
        // A phone carrying IPv6 next to a cable carrying IPv4: only the cable is the usual one.
        let phone = ("usb0".to_string(), vec![v4(10, 112, 106, 144), "2c0f:f290::1".parse().unwrap()]);
        let (found, tunnel) = shape(vec![cable.clone(), phone], Some(v4(10, 0, 0, 5)), Some("2c0f:f290::1".parse().unwrap()));
        assert!(!tunnel);
        assert_eq!(found.iter().filter(|f| f.main).map(|f| f.spec.name.as_str()).collect::<Vec<_>>(), ["Ethernet"]);
        // Offline: no route at all is not a tunnel.
        assert!(!shape(vec![wifi, cable], None, None).1);
    }

    #[tokio::test]
    async fn a_reading_that_hangs_is_not_waited_for_and_never_piles_up() {
        let reading = Arc::new(std::sync::atomic::AtomicBool::new(false));
        assert_eq!(read_bounded(&reading, Duration::from_secs(1), || 7).await, Some(7));
        let ran = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let r2 = ran.clone();
        let t = Instant::now();
        let slow = read_bounded(&reading, Duration::from_millis(100), move || {
            r2.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(600));
        })
        .await;
        assert!(slow.is_none() && t.elapsed() < Duration::from_millis(400), "not waited for");
        // While it is stuck, no other reading starts.
        for _ in 0..5 {
            let r3 = ran.clone();
            assert!(read_bounded(&reading, Duration::from_secs(1), move || r3.fetch_add(1, Ordering::SeqCst)).await.is_none());
        }
        assert_eq!(ran.load(Ordering::SeqCst), 1, "one thread only");
        // Once it ends, readings work again.
        tokio::time::sleep(Duration::from_millis(700)).await;
        assert_eq!(read_bounded(&reading, Duration::from_secs(1), || 8).await, Some(8));
    }

    #[test]
    fn saved_names_are_checked() {
        assert!(valid_name("Wi-Fi") && valid_name("Ethernet 2"));
        assert!(!valid_name("") && !valid_name("a\nb") && !valid_name(&"x".repeat(MAX_NAME + 1)));
    }

    #[test]
    fn this_computer_is_read_without_failing() {
        // Whatever the machine has (CI sandboxes may have no adapter at all): no virtual adapter, no unusable address.
        let (found, _, _) = discover(HashMap::new());
        for f in &found {
            assert!(!is_virtual(&f.spec.name));
            assert!(!f.spec.addresses.is_empty() && f.spec.addresses.iter().all(|a| usable(*a)));
        }
    }
}
