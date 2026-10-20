//! What kind of network adapter each one is, to name it plainly ("Wi-Fi", "USB Ethernet", "Phone (USB)"...) instead of
//! the system's identifier (`wlx00c0ca98ff70`, `enx1e4a6e01b405`, `en7`). Read only from the system's own description
//! of its adapters: nothing is sent.
//!
//! * Linux: `/sys/class/net/<name>`: `wireless` / `phy80211`, `DEVTYPE` (`wwan`, `bluetooth`), the driver
//!   (`rndis_host`, `ipheth`: a phone sharing its connection; `cdc_ether` / `cdc_ncm`: a phone or a USB Ethernet
//!   adapter, told apart by the USB device's product name), and whether it sits on a USB bus.
//! * Windows: the adapter's type and description (`GetIfTable2`): IEEE 802.11, WWAN, "Remote NDIS" / "Apple Mobile
//!   Device" (a phone), "USB" (a USB Ethernet adapter), "Bluetooth".
//! * macOS: the system's own port names (`networksetup -listallhardwareports`, fixed arguments, no shell): "Wi-Fi",
//!   "iPhone USB", "USB 10/100/1000 LAN", "Thunderbolt Ethernet", "Bluetooth PAN"...
//!
//! What cannot be told is `Other` (the app then shows the system's name).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AdapterKind {
    Wifi,
    Ethernet,
    UsbEthernet,
    /// A phone sharing its connection over USB.
    Phone,
    /// A mobile broadband modem (built-in or a USB stick).
    Mobile,
    Bluetooth,
    Other,
}

/// Product names of phones (a USB device's `product`), in lower case.
const PHONES: [&str; 18] = [
    "android", "iphone", "ipad", "pixel", "galaxy", "samsung", "xiaomi", "redmi", "poco", "huawei", "honor", "oneplus", "motorola", "oppo",
    "vivo", "realme", "nokia", "phone",
];

fn is_phone_product(product: &str) -> bool {
    let p = product.to_ascii_lowercase();
    PHONES.iter().any(|w| p.contains(w))
}

/// Linux: what `/sys/class/net/<name>` says. `exists(path)` and `read(path)` are relative to that folder; `driver` is the
/// adapter's driver name, `usb` whether it sits on a USB bus, `product` the USB device's product name.
pub fn classify_linux(
    exists: impl Fn(&str) -> bool,
    read: impl Fn(&str) -> Option<String>,
    driver: Option<&str>,
    usb: bool,
    product: Option<&str>,
) -> AdapterKind {
    let uevent = read("uevent").unwrap_or_default();
    let devtype = uevent.lines().find_map(|l| l.trim().strip_prefix("DEVTYPE=")).unwrap_or("");
    if exists("wireless") || exists("phy80211") || devtype == "wlan" {
        return AdapterKind::Wifi;
    }
    match driver.unwrap_or("") {
        "rndis_host" | "ipheth" => return AdapterKind::Phone,
        "qmi_wwan" | "cdc_mbim" | "huawei_cdc_ncm" | "option" => return AdapterKind::Mobile,
        "bnep" => return AdapterKind::Bluetooth,
        "cdc_ether" | "cdc_ncm" if product.is_some_and(is_phone_product) => return AdapterKind::Phone,
        _ => {}
    }
    match devtype {
        "wwan" => return AdapterKind::Mobile,
        "bluetooth" => return AdapterKind::Bluetooth,
        "" => {}
        _ => return AdapterKind::Other, // bridge, vlan, tunnel...
    }
    if !exists("device") || read("type").is_none_or(|t| t.trim() != "1") {
        return AdapterKind::Other; // virtual
    }
    if usb {
        AdapterKind::UsbEthernet
    } else {
        AdapterKind::Ethernet
    }
}

/// Windows: an adapter's interface type and description.
pub fn classify_windows(if_type: u32, description: &str) -> AdapterKind {
    let d = description.to_ascii_lowercase();
    if d.contains("remote ndis") || d.contains("apple mobile device") || d.contains("rndis") || is_phone_product(&d) {
        return AdapterKind::Phone;
    }
    if d.contains("bluetooth") {
        return AdapterKind::Bluetooth;
    }
    match if_type {
        71 => AdapterKind::Wifi,
        243 | 244 => AdapterKind::Mobile,
        6 if d.contains("usb") => AdapterKind::UsbEthernet,
        6 => AdapterKind::Ethernet,
        _ => AdapterKind::Other,
    }
}

/// macOS: a hardware port's name (`networksetup -listallhardwareports`).
pub fn classify_mac_port(port: &str) -> AdapterKind {
    let p = port.to_ascii_lowercase();
    if p.contains("wi-fi") || p.contains("airport") {
        AdapterKind::Wifi
    } else if p.contains("iphone") || p.contains("ipad") || is_phone_product(&p) {
        AdapterKind::Phone
    } else if p.contains("bluetooth") {
        AdapterKind::Bluetooth
    } else if p.contains("bridge") {
        AdapterKind::Other
    } else if p.contains("usb") && (p.contains("lan") || p.contains("ethernet")) {
        AdapterKind::UsbEthernet
    } else if p.contains("ethernet") || p.contains("lan") {
        AdapterKind::Ethernet
    } else if p.contains("modem") || p.contains("wwan") || p.contains("cellular") {
        AdapterKind::Mobile
    } else {
        AdapterKind::Other
    }
}

/// `networksetup -listallhardwareports` output: device name -> port name.
pub fn parse_mac_ports(text: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut port: Option<String> = None;
    for line in text.lines() {
        if let Some(p) = line.strip_prefix("Hardware Port:") {
            port = Some(p.trim().to_string());
        } else if let (Some(d), Some(p)) = (line.strip_prefix("Device:"), port.take()) {
            out.insert(d.trim().to_string(), p);
        }
    }
    out
}

/// The kind of each named adapter (the names `sysinfo` lists: interface names on Linux / macOS, aliases on Windows).
/// Unknown names are `Other`. Reads the system: call it off the async threads.
pub fn adapter_kinds(names: &[String]) -> HashMap<String, AdapterKind> {
    #[cfg(target_os = "linux")]
    return names.iter().map(|n| (n.clone(), linux::kind(n))).collect();
    #[cfg(windows)]
    return windows::kinds(names);
    #[cfg(target_os = "macos")]
    return mac::kinds(names);
    #[allow(unreachable_code)]
    names.iter().map(|n| (n.clone(), AdapterKind::Other)).collect()
}

#[cfg(target_os = "linux")]
mod linux {
    use std::path::Path;

    use super::*;

    pub fn kind(name: &str) -> AdapterKind {
        // A name from the system, used as one path part only.
        if name.is_empty() || name.contains('/') || name.contains("..") {
            return AdapterKind::Other;
        }
        let dir = Path::new("/sys/class/net").join(name);
        let device = std::fs::canonicalize(dir.join("device")).ok();
        let driver = std::fs::read_link(dir.join("device").join("driver"))
            .ok()
            .and_then(|p| p.file_name().map(|f| f.to_string_lossy().into_owned()));
        let usb = device.as_ref().is_some_and(|d| d.components().any(|c| c.as_os_str().to_string_lossy().starts_with("usb")));
        // The USB device (the interface's parent) names the product: "SAMSUNG_Android", "iPhone", "AX88179"...
        let product = device.as_ref().and_then(|d| d.parent()).and_then(|p| std::fs::read_to_string(p.join("product")).ok());
        classify_linux(
            |p| dir.join(p).exists(),
            |p| std::fs::read_to_string(dir.join(p)).ok(),
            driver.as_deref(),
            usb,
            product.as_deref().map(str::trim),
        )
    }
}

#[cfg(windows)]
mod windows {
    use windows_sys::Win32::NetworkManagement::IpHelper::{FreeMibTable, GetIfTable2, MIB_IF_TABLE2};

    use super::*;

    fn wide(s: &[u16]) -> String {
        let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
        String::from_utf16_lossy(&s[..end])
    }

    pub fn kinds(names: &[String]) -> HashMap<String, AdapterKind> {
        let mut by_alias: HashMap<String, AdapterKind> = HashMap::new();
        let mut table: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
        if unsafe { GetIfTable2(&mut table) } == 0 && !table.is_null() {
            let t = unsafe { &*table };
            let rows = unsafe { std::slice::from_raw_parts(t.Table.as_ptr(), t.NumEntries as usize) };
            for r in rows {
                by_alias.entry(wide(&r.Alias)).or_insert_with(|| classify_windows(r.Type, &wide(&r.Description)));
            }
            unsafe { FreeMibTable(table as *const _) };
        }
        names.iter().map(|n| (n.clone(), by_alias.get(n).copied().unwrap_or(AdapterKind::Other))).collect()
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use super::*;

    pub fn kinds(names: &[String]) -> HashMap<String, AdapterKind> {
        let ports = std::process::Command::new("/usr/sbin/networksetup")
            .arg("-listallhardwareports")
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| parse_mac_ports(&String::from_utf8_lossy(&o.stdout)))
            .unwrap_or_default();
        names.iter().map(|n| (n.clone(), ports.get(n).map_or(AdapterKind::Other, |p| classify_mac_port(p)))).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn linux(files: &[(&str, &str)], driver: Option<&str>, usb: bool, product: Option<&str>) -> AdapterKind {
        let f: HashMap<String, String> = files.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        classify_linux(|p| f.contains_key(p), |p| f.get(p).cloned(), driver, usb, product)
    }

    #[test]
    fn linux_adapters_are_named_by_what_they_are() {
        let eth = [("type", "1\n"), ("device", ""), ("uevent", "INTERFACE=x\n")];
        assert_eq!(linux(&eth, Some("r8169"), false, None), AdapterKind::Ethernet);
        assert_eq!(linux(&eth, Some("r8152"), true, Some("USB 10/100/1000 LAN")), AdapterKind::UsbEthernet);
        assert_eq!(linux(&eth, Some("ax88179_178a"), true, Some("AX88179")), AdapterKind::UsbEthernet);
        assert_eq!(
            linux(&eth, Some("cdc_ether"), true, Some("AX88179A")),
            AdapterKind::UsbEthernet,
            "a USB Ethernet with a standard driver"
        );
        assert_eq!(linux(&eth, Some("rndis_host"), true, Some("SAMSUNG_Android")), AdapterKind::Phone);
        assert_eq!(linux(&eth, Some("cdc_ncm"), true, Some("Pixel 8")), AdapterKind::Phone);
        assert_eq!(linux(&eth, Some("ipheth"), true, None), AdapterKind::Phone);
        let wifi = [("type", "1"), ("device", ""), ("wireless", ""), ("uevent", "DEVTYPE=wlan\n")];
        assert_eq!(linux(&wifi, Some("ath9k_htc"), true, None), AdapterKind::Wifi, "a USB Wi-Fi is Wi-Fi");
        assert_eq!(linux(&[("phy80211", "")], Some("iwlwifi"), false, None), AdapterKind::Wifi);
        assert_eq!(
            linux(&[("type", "1"), ("device", ""), ("uevent", "DEVTYPE=wwan\n")], Some("cdc_mbim"), true, None),
            AdapterKind::Mobile
        );
        assert_eq!(
            linux(&[("type", "1"), ("device", ""), ("uevent", "DEVTYPE=bluetooth\n")], Some("bnep"), false, None),
            AdapterKind::Bluetooth
        );
        assert_eq!(linux(&[("type", "1"), ("uevent", "DEVTYPE=bridge\n")], None, false, None), AdapterKind::Other);
        assert_eq!(linux(&[("type", "1")], None, false, None), AdapterKind::Other, "virtual: no device");
    }

    #[test]
    fn windows_adapters_are_named_by_what_they_are() {
        assert_eq!(classify_windows(71, "Intel(R) Wi-Fi 6 AX201 160MHz"), AdapterKind::Wifi);
        assert_eq!(classify_windows(6, "Realtek PCIe GbE Family Controller"), AdapterKind::Ethernet);
        assert_eq!(classify_windows(6, "Realtek USB GbE Family Controller"), AdapterKind::UsbEthernet);
        assert_eq!(classify_windows(6, "Remote NDIS based Internet Sharing Device"), AdapterKind::Phone);
        assert_eq!(classify_windows(6, "Apple Mobile Device Ethernet"), AdapterKind::Phone);
        assert_eq!(classify_windows(243, "Generic Mobile Broadband Adapter"), AdapterKind::Mobile);
        assert_eq!(classify_windows(6, "Bluetooth Device (Personal Area Network)"), AdapterKind::Bluetooth);
        assert_eq!(classify_windows(53, "Hyper-V Virtual Ethernet Adapter"), AdapterKind::Other);
    }

    #[test]
    fn mac_ports_are_named_by_what_they_are() {
        let ports = parse_mac_ports(
            "\nHardware Port: Wi-Fi\nDevice: en0\nEthernet Address: aa\n\nHardware Port: iPhone USB\nDevice: en7\n\nHardware Port: USB 10/100/1000 LAN\nDevice: en8\n\nHardware Port: Thunderbolt Bridge\nDevice: bridge0\n",
        );
        assert_eq!(ports.get("en0").map(|p| classify_mac_port(p)), Some(AdapterKind::Wifi));
        assert_eq!(ports.get("en7").map(|p| classify_mac_port(p)), Some(AdapterKind::Phone));
        assert_eq!(ports.get("en8").map(|p| classify_mac_port(p)), Some(AdapterKind::UsbEthernet));
        assert_eq!(ports.get("bridge0").map(|p| classify_mac_port(p)), Some(AdapterKind::Other));
        assert_eq!(classify_mac_port("Thunderbolt Ethernet Slot 0"), AdapterKind::Ethernet);
        assert_eq!(classify_mac_port("Bluetooth PAN"), AdapterKind::Bluetooth);
    }

    #[test]
    fn this_computer_is_read_without_failing() {
        let names: Vec<String> = std::fs::read_dir("/sys/class/net")
            .map(|d| d.filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned())).collect())
            .unwrap_or_default();
        eprintln!("{:?}", adapter_kinds(&names));
    }
}
