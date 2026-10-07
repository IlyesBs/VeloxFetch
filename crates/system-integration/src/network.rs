//! Is the computer online, and over which kind of connection? For the download queue's rules ("only on
//! Wi-Fi", "not on mobile data") and so that downloads are not started while there is no network.
//!
//! Only the routing table and the adapter type are read: nothing is sent, no address is contacted. What a
//! system does not say is `None`, and the queue never holds downloads back for something unknown.
//!
//! * Linux: the default route in `/proc/net/route` (or `/proc/net/ipv6_route`), and the adapter in
//!   `/sys/class/net/<name>` (`wireless` = Wi-Fi, a WWAN device = mobile, a physical Ethernet = wired).
//! * Windows: the adapter the system would use to reach the Internet (`GetBestInterface`) and its type.
//! * macOS: whether the system can reach the Internet (`SCNetworkReachability` on the default route, asked
//!   synchronously: no socket is opened); the connection type is not read.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConnectionKind {
    Wifi,
    Wired,
    Mobile,
    /// A VPN, a virtual adapter, or anything else.
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct NetworkState {
    /// `Some(false)`: no route to the Internet. `None`: not known.
    pub online: Option<bool>,
    pub kind: Option<ConnectionKind>,
}

/// The adapter of the IPv4 default route in `/proc/net/route` text.
pub fn default_route_v4(route_table: &str) -> Option<String> {
    route_table.lines().skip(1).find_map(|line| {
        let f: Vec<&str> = line.split_whitespace().collect();
        let (iface, dest, flags, mask) = (f.first()?, f.get(1)?, f.get(3)?, f.get(7)?);
        let up = u32::from_str_radix(flags, 16).ok()? & 1 == 1;
        (up && *dest == "00000000" && *mask == "00000000" && *iface != "lo").then(|| iface.to_string())
    })
}

/// The adapter of the IPv6 default route in `/proc/net/ipv6_route` text.
pub fn default_route_v6(route_table: &str) -> Option<String> {
    route_table.lines().find_map(|line| {
        let f: Vec<&str> = line.split_whitespace().collect();
        let (dest, prefix, flags, iface) = (f.first()?, f.get(1)?, f.get(8)?, f.get(9)?);
        let flags = u32::from_str_radix(flags, 16).ok()?;
        // RTF_UP and not a rejected ("unreachable") route.
        let usable = flags & 0x1 == 1 && flags & 0x200 == 0;
        (usable && dest.bytes().all(|b| b == b'0') && *prefix == "00" && *iface != "lo").then(|| iface.to_string())
    })
}

/// What `/sys/class/net/<name>` says about an adapter. `exists(path)` and `read(path)` work relative to that
/// folder (kept apart for the tests).
pub fn classify_adapter(name: &str, exists: impl Fn(&str) -> bool, read: impl Fn(&str) -> Option<String>) -> ConnectionKind {
    if exists("wireless") || exists("phy80211") {
        return ConnectionKind::Wifi;
    }
    let uevent = read("uevent").unwrap_or_default();
    if name.starts_with("ww") || uevent.lines().any(|l| l.trim() == "DEVTYPE=wwan") {
        return ConnectionKind::Mobile;
    }
    // A physical Ethernet card (virtual ones - bridges, containers - have no `device`).
    if read("type").is_some_and(|t| t.trim() == "1") && exists("device") && !uevent.lines().any(|l| l.starts_with("DEVTYPE=")) {
        return ConnectionKind::Wired;
    }
    ConnectionKind::Other
}

/// The connection right now. Cheap (a few small file reads or one system call); safe from any thread.
pub fn current() -> NetworkState {
    #[cfg(target_os = "linux")]
    return linux::current();
    #[cfg(windows)]
    return windows::current();
    #[cfg(target_os = "macos")]
    return NetworkState { online: mac::reachable(), kind: None };
    #[allow(unreachable_code)]
    NetworkState::default()
}

#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::c_void;

    /// `struct sockaddr_in` as macOS lays it out.
    #[repr(C)]
    struct SockaddrIn {
        sin_len: u8,
        sin_family: u8,
        sin_port: u16,
        sin_addr: u32,
        sin_zero: [u8; 8],
    }

    #[link(name = "SystemConfiguration", kind = "framework")]
    extern "C" {
        fn SCNetworkReachabilityCreateWithAddress(allocator: *const c_void, address: *const SockaddrIn) -> *const c_void;
        fn SCNetworkReachabilityGetFlags(target: *const c_void, flags: *mut u32) -> u8;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFRelease(cf: *const c_void);
    }

    const REACHABLE: u32 = 1 << 1;
    const CONNECTION_REQUIRED: u32 = 1 << 2;
    const AF_INET: u8 = 2;

    /// Can the default route reach the Internet? (The address 0.0.0.0 stands for "the Internet in general".)
    pub fn reachable() -> Option<bool> {
        let any = SockaddrIn {
            sin_len: std::mem::size_of::<SockaddrIn>() as u8,
            sin_family: AF_INET,
            sin_port: 0,
            sin_addr: 0,
            sin_zero: [0; 8],
        };
        unsafe {
            let target = SCNetworkReachabilityCreateWithAddress(std::ptr::null(), &any);
            if target.is_null() {
                return None;
            }
            let mut flags = 0u32;
            let ok = SCNetworkReachabilityGetFlags(target, &mut flags) != 0;
            CFRelease(target);
            ok.then_some(flags & REACHABLE != 0 && flags & CONNECTION_REQUIRED == 0)
        }
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::path::Path;

    use super::*;

    pub fn current() -> NetworkState {
        let v4 = std::fs::read_to_string("/proc/net/route").ok();
        let v6 = std::fs::read_to_string("/proc/net/ipv6_route").ok();
        if v4.is_none() && v6.is_none() {
            return NetworkState::default(); // not readable here: unknown
        }
        let iface = v4.as_deref().and_then(default_route_v4).or_else(|| v6.as_deref().and_then(default_route_v6));
        let Some(iface) = iface else { return NetworkState { online: Some(false), kind: None } };
        // A name from the routing table, used as one path part only.
        if iface.contains('/') || iface.contains("..") {
            return NetworkState { online: Some(true), kind: None };
        }
        let dir = Path::new("/sys/class/net").join(&iface);
        let kind = classify_adapter(&iface, |p| dir.join(p).exists(), |p| std::fs::read_to_string(dir.join(p)).ok());
        NetworkState { online: Some(true), kind: Some(kind) }
    }
}

#[cfg(windows)]
mod windows {
    use windows_sys::Win32::NetworkManagement::IpHelper::{GetBestInterface, GetIfEntry2, MIB_IF_ROW2};

    use super::*;

    const IF_TYPE_ETHERNET_CSMACD: u32 = 6;
    const IF_TYPE_IEEE80211: u32 = 71;
    const IF_TYPE_WWANPP: u32 = 243;
    const IF_TYPE_WWANPP2: u32 = 244;
    const ERROR_NETWORK_UNREACHABLE: u32 = 1231;
    const ERROR_HOST_UNREACHABLE: u32 = 1232;

    pub fn current() -> NetworkState {
        let mut index = 0u32;
        // 8.8.8.8, in network byte order: only the routing table is asked.
        let err = unsafe { GetBestInterface(u32::from_ne_bytes([8, 8, 8, 8]), &mut index) };
        if err != 0 {
            let offline = matches!(err, ERROR_NETWORK_UNREACHABLE | ERROR_HOST_UNREACHABLE);
            return NetworkState { online: offline.then_some(false), kind: None };
        }
        let mut row: MIB_IF_ROW2 = unsafe { std::mem::zeroed() };
        row.InterfaceIndex = index;
        let kind = if unsafe { GetIfEntry2(&mut row) } == 0 {
            Some(match row.Type {
                IF_TYPE_IEEE80211 => ConnectionKind::Wifi,
                IF_TYPE_ETHERNET_CSMACD => ConnectionKind::Wired,
                IF_TYPE_WWANPP | IF_TYPE_WWANPP2 => ConnectionKind::Mobile,
                _ => ConnectionKind::Other,
            })
        } else {
            None
        };
        NetworkState { online: Some(true), kind }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn the_default_route_is_read_from_the_routing_tables() {
        let v4 = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n\
                  docker0\t0011A8C0\t00000000\t0001\t0\t0\t0\t00FFFFFF\t0\t0\t0\n\
                  wlp2s0\t00000000\t0101A8C0\t0003\t0\t0\t600\t00000000\t0\t0\t0\n";
        assert_eq!(default_route_v4(v4).as_deref(), Some("wlp2s0"));
        let none = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n\
                    eth0\t0011A8C0\t00000000\t0001\t0\t0\t0\t00FFFFFF\t0\t0\t0\n";
        assert_eq!(default_route_v4(none), None, "no default route: offline");
        let down = "h\neth0\t00000000\t0101A8C0\t0002\t0\t0\t0\t00000000\t0\t0\t0\n";
        assert_eq!(default_route_v4(down), None, "a route that is not up does not count");
        assert_eq!(default_route_v4(""), None);

        let v6 = "fe800000000000000000000000000000 40 00000000000000000000000000000000 00 00000000000000000000000000000000 00000100 00000001 00000000 00000001 wlp2s0\n\
                  00000000000000000000000000000000 00 00000000000000000000000000000000 00 fe80000000000000022c29fffe8a5b6c 00000400 00000002 00000000 00000003 enp3s0\n";
        assert_eq!(default_route_v6(v6).as_deref(), Some("enp3s0"));
        let unreachable = "00000000000000000000000000000000 00 00000000000000000000000000000000 00 00000000000000000000000000000000 ffffffff 00000001 00000000 00200200 lo\n";
        assert_eq!(default_route_v6(unreachable), None);
    }

    #[test]
    fn adapters_are_told_apart() {
        let fs = |files: &[(&str, &str)]| files.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<HashMap<_, _>>();
        let kind =
            |name: &str, files: HashMap<String, String>| classify_adapter(name, |p| files.contains_key(p), |p| files.get(p).cloned());
        assert_eq!(kind("wlp2s0", fs(&[("wireless", ""), ("type", "1"), ("device", "")])), ConnectionKind::Wifi);
        assert_eq!(kind("wlan0", fs(&[("phy80211", "")])), ConnectionKind::Wifi);
        assert_eq!(
            kind("enp3s0", fs(&[("type", "1\n"), ("device", ""), ("uevent", "INTERFACE=enp3s0\nIFINDEX=2\n")])),
            ConnectionKind::Wired
        );
        assert_eq!(kind("wwan0", fs(&[("type", "65534")])), ConnectionKind::Mobile);
        assert_eq!(kind("usb0", fs(&[("type", "1"), ("device", ""), ("uevent", "DEVTYPE=wwan\n")])), ConnectionKind::Mobile);
        assert_eq!(kind("tun0", fs(&[("type", "65534")])), ConnectionKind::Other, "a VPN is not known");
        assert_eq!(kind("br0", fs(&[("type", "1"), ("uevent", "DEVTYPE=bridge\n")])), ConnectionKind::Other);
        assert_eq!(kind("veth1", fs(&[("type", "1")])), ConnectionKind::Other, "virtual: no device");
    }

    #[test]
    fn reading_the_real_connection_never_fails() {
        let s = current();
        eprintln!("this computer: {s:?}");
        if s.online == Some(false) {
            assert_eq!(s.kind, None);
        }
    }
}
