# Several networks at once (October 2026)

Settings > Network > **Several networks at once**: a download's connections are spread over the computer's networks
(Wi-Fi and a cable, a phone shared over USB...), so their speeds add up. It is off until the user turns it on, and each
network can be left out (a phone with a data limit, for example).

## 1. Before

Every connection went out the way the system sends everything: through one network. A second connected network (the
phone next to a slow Wi-Fi, a cable next to Wi-Fi) stayed unused.

## 2. What it does now

* **The networks** are read from the system: adapters with a hardware address, up, with an address that can reach
  the internet (not a link-local one), and not a virtual adapter (containers, virtual machines, tunnels, packet
  capture: `docker*`, `veth*`, `vEthernet`, `VirtualBox`, `VMware`, `tun*`, `tap*`, `wg*`, `utun*`, `tailscale`,
  `ZeroTier`, `NordLynx`, `OpenVPN`, names with `vpn`, `Npcap`, `lo`...). The one the system's own route to the
  internet leaves from is marked "usual". They are read again every 5 s while it is on: a network that comes is
  used within seconds; one that goes is dropped and **its transfers end at once** (their parts continue on the
  others), instead of waiting for them to stall. One reading at a time, waited for at most 2 s (a network driver that hangs in the
  kernel can block every network call of the system: the last reading is kept and no thread piles up).
* **Plain names** (`system_integration::adapters`): each network is shown by what it is, in the user's language:
  "Wi-Fi", "Ethernet", "USB Ethernet", "Phone (USB)", "Mobile network", "Bluetooth" ("Wi-Fi 2" for a second one of a
  kind); the system's name only when the kind is unknown (and in the tooltip). Linux: `/sys/class/net` (`wireless`,
  `DEVTYPE`, the driver: `rndis_host` / `ipheth` a phone, `cdc_ether` / `cdc_ncm` a phone or a USB Ethernet adapter
  by the USB product name, a USB bus); Windows: the adapter's type and description (`GetIfTable2`: 802.11, WWAN,
  "Remote NDIS" / "Apple Mobile Device", "USB", "Bluetooth"); macOS: `networksetup -listallhardwareports` (fixed
  arguments, no shell), read once per adapter.
* **Smart throttle is held off** while it is on: every network at full speed. The user's own Smart throttle switch
  (and a profile's automatic mode) is kept and comes back when it is turned off; meanwhile Smart throttle cannot be
  turned on (refused, its switches greyed out with "Off while several networks at once is on").
* **Spreading**: each chosen network gets its own connections, bound to it (Linux `SO_BINDTODEVICE`, macOS
  `IP_BOUND_IF`, Windows its own address: the system then sends through it). A request goes to the network with the
  least work for its speed: open connections per measured speed **per connection** (smoothed, measured only while it
  has connections; per connection, because a network given fewer connections would look slower in total, get fewer
  still and never show what it can do), never below 16 KiB/s or a tenth of the fastest; a network not measured yet
  counts as fast as the fastest, so it is tried at once. Aggressive: 8 connections per network from the start, never
  fewer than 4 per network (only the server's own "too many" goes below; the user's connection limit stays the
  ceiling), climbing to 64 as usual.
* **Every network at its own full speed, to the end of the file**:
  * requests of about 4 s of transfer (512 KiB to 16 MiB, sized by the part's last request), no runs of parts:
    connections are handed out again every few seconds, so a network that comes back or a faster one gets its share
    at once, and a slow network never holds much of the end;
  * a transfer that crawls (under an eighth of the best network's speed per connection, over 4 s of waiting for
    bytes) is ended without resting its network, and its part continues at once on a faster one;
  * a transfer that receives nothing for 10 s gives up and its network rests;
  * a lost transfer on one network (`HttpError::NetworkLost`) is retried on another and shown in the diagnostics,
    but never counted as a server error by the connection control (which would halve the connections of every
    network for one weak Wi-Fi).
* **Only where it is safe** (`http_client::spreadable`): https links (the certificate proves it is the same server
  whatever the network), to a public address, without a proxy, and only when the system itself would send that
  server's requests over one of the networks (see §4). Everything else goes the usual way, as before.
* **Never a failure because of it**: a network that cannot connect, breaks a transfer or presents another
  certificate rests (5 s, doubled each time it fails again after a rest, up to 5 min; all its connections breaking
  at once when it is unplugged count as one failure) and its request goes the usual way at once,
  without spending a retry. A server that refuses a network (`401`, `403`, `404`, `410`, `451`: a link signed for
  another address...) is not asked through it again for 10 minutes; the same request goes the usual way. When no
  chosen network can be used, everything goes the usual way.
* **Fewer than two chosen networks**: nothing is spread.
* **The network list is stable**: a network read again with another IPv6 temporary address is the same network (it
  keeps its connections and what was learned about it); "usual" is the one the system's IPv4 route leaves from (its
  IPv6 one only without IPv4: a phone often carries IPv6 next to a cable carrying IPv4).

## Measured on a real computer (October 2026)

Linux, three networks: a cable and a USB Wi-Fi (Atheros AR9271) on the same home router, and a phone shared over USB
(mobile data). A 145 MB file from `cdn.kernel.org` (a CDN that serves every connection), `tests/integration/tests/multinet_real.rs`
and `tests/e2e/real-multinet.mjs`; the system's own byte counters (`/proc/net/dev`) confirm what the app says came
through each network.

| | Time | Speed |
|---|---|---|
| Each alone, 8 connections (`curl`) | | cable 3.3, phone 3.0, Wi-Fi 1.0 MB/s |
| All three at once (`curl`, 8 connections each) | | 5.7 MB/s (cable + Wi-Fi share one router line) |
| VeloxFetch, the usual route | 52.5 s | 2.76 MB/s |
| VeloxFetch, spread over the three | 32 – 37.6 s | 3.9 – 4.5 MB/s average, about 5 MB/s steady (cable ≈ 2.4, phone ≈ 2.2, Wi-Fi ≈ 1.1) |
| Later, the phone on a faster signal | 14 s | 10.4 MB/s average, 12 – 14 MB/s steady |
| Later again, all three slower (`curl` reference before / after: 2.85 / 3.1 MB/s) | 46 s | 3.3 – 3.5 MB/s steady: at least what `curl` gets with 8 connections on each |

**Switching**, measured with `multinet_real.rs` (`switching_...`: each network disconnected with `nmcli` during a 4 GB
download for 17 s, then connected again) and `show_one_network_going_and_coming_back` (every 2 s): the download never
stopped (the longest wait for a byte: 1.5 s, the polling step); a disconnected network's 21 connections moved to the
others within 2 s with no drop in speed; a network that came back was used again within 2 – 4 s (the USB Wi-Fi takes
10 – 15 s to join its router again by itself); "VPN" was never shown wrongly.

The first real runs found what the loopback tests could not, all fixed and covered by tests since: networks never
measured were never tried; the list was rebuilt every few seconds; two networks were marked "usual"; one weak Wi-Fi's
lost transfers halved every network's connections (24 to 3); the end of a file waited on long runs held by the slow
networks; a crawling transfer held the last part; the names were the system's identifiers; a network coming back
waited up to 5 minutes (all its broken connections counted as failures in a row) and minutes more for a whole part to
end somewhere; a network going away left its transfers hanging for 10 s; the IPv6 route a router announces to a
computer without an IPv6 address of its own (`fe80::` only) made every server look "reached another way", and a
network going away briefly looked like a VPN (spreading stopped for about 50 s). The USB Wi-Fi's driver also froze
inside the kernel once (every network command of the system hung, `curl` too): the app gave that network up and
finished on the others.

## 3. How it shows

* Settings > Network: the switch, the rule ("only for https links to the internet"), each network by its plain name
  ("Ethernet", "Wi-Fi", "Phone (USB)"...) with a check box, "usual" on the system's own one, and while on: "Ready", "In use" or "Not answering, tried again soon",
  and what it received since the app started. A VPN or a proxy shows why nothing is spread; one network shows
  "connect another one"; one chosen shows "choose at least two". The list is read again every 3 s while Settings is
  open.
* A download's details: "Networks: Wi-Fi 6.0 MB · Ethernet 6.0 MB" (what came through each, over every attempt, by
  plain name); the copied report says "Wi-Fi [wlx…] 6.0 MB" (the system's name in brackets, for support).
* Smart throttle (sidebar card and Settings): greyed out, "Off while several networks at once is on".
* Names only, everywhere (UI, report, log): never an address.

## 4. Design

| Part | Where |
|---|---|
| Lanes (one client per network), choice, rests, refusals, the per-server route check (`system_source_for`) | `crates/http-client/src/lanes.rs` |
| Spreading a ranged request, falling back, the shared cookie jar | `crates/http-client/src/lib.rs` (`range_on`, `set_routes`, `build_client`) |
| Bytes per network per run | `downloader-core` `ProgressHandle::networks` |
| Reading the networks, the setting, VPN / proxy detection | `crates/download-manager/src/manager/multinet.rs` |
| Bytes per network per download | `manager/diag.rs` (`DiagRecord::networks`, `DiagnosticsDto::networks`) |
| Commands `get_multi_network`, `set_multi_network` | `apps/desktop/src-tauri` (`lib.rs`, `authz.rs`, `build.rs`, `capabilities/main-window.json`) |
| Settings panel, details line | `MultiNetworkSettings.tsx`, `DiagnosticsCard.tsx` |

* **The system's route decides** (`Lanes::system_route_is_ours`): before a server's requests are spread, the system
  is asked which address it would send to each of the server's addresses from (addresses it has no route to, or only
  a link-local one, are skipped: nothing goes there; a UDP socket "connected" to it: no packet is sent). Only if that address belongs to one of the networks is anything spread, so a VPN that carries
  everything, or only some addresses (split tunnel), is never gone around. Kept a minute per server, forgotten when
  the networks change. The same question at reading time (to documentation addresses; IPv4 decides while there is
  an IPv4 route, and two readings in a row must agree) tells Settings that a VPN carries the traffic; it only
  changes the message, never the networks (each server's own check keeps a VPN's traffic on it, and a network going
  away, its routes changing for a moment, must not stop spreading for the others).
* **The same checks on every network**: a ranged answer through a network goes through exactly the same checks as
  one the usual way (status `206`, no compression, `Content-Range` start / end / total, `If-Range`, the strong ETag),
  and the joins between parts are checked as before (`docs/resume-integrity.md`).
* **Shared**: one names cache for the networks, and one cookie jar for every route (a cookie the server set on the
  usual route goes with the requests through the networks). The headers, the user agent, the redirect rules and the
  limits are the same.
* **A network's family**: by address (Windows), a network only reaches servers of its address's family (the other
  family's addresses are not tried through it); by interface, both when it has both.
* **Proxy**: a proxy set in Settings, or one in the environment (`HTTPS_PROXY`, `ALL_PROXY`) with the system's
  settings, stops spreading.
* **Saved**: `network.multi` (`1` / `0`) and `network.multi_skip` (a JSON list of names, at most 64, each at most 256
  printable characters). Damaged values are ignored.

## 5. Safety

* Off by default; the user chooses the networks.
* https only: the certificate proves the server on every network, so a hostile network (public Wi-Fi) cannot change
  the bytes without failing the TLS check (then that network rests).
* Never around a VPN or a proxy (§4).
* Local addresses (`localhost`, private ranges, `.local`...) are never spread: they are reached from one network.
* Credentials (cookies, sign-in) go only to the same server, as on the usual route; nothing new is sent anywhere.
* No address is shown, saved, reported or logged; only names.
* The route question is the only datagram socket in shipped code: never bound by hand, never sent on, read or
  listening, closed at once (`socket2`, connect only); `tests/security/policy.test.mjs` holds it to that and still
  refuses any other socket outside the preview address.
* The input is checked (`deny_unknown_fields`, names checked); a refused change saves nothing.
* The test hooks (`ClientConfig::spread_any_link`, `ManagerConfig::test_networks`, `VELOX_TEST_NETWORKS`) are never
  set by the app; the environment variable is read by debug builds only.

## 6. Tests

* `http-client` (unit): only https to public addresses without a proxy; one network is not spread and requests go
  to the least busy; a failed network rests (longer at each failure, never beyond 5 min) and a refusal is per server;
  a faster network gets more requests and a slow one still some; one family per address-bound network; a server the
  system reaches another way is never spread (kept a minute, forgotten on a change); a request through a network
  carries the cookie set on the usual route and is checked like any other (fails without the shared jar).
* `download-manager` (unit): virtual adapters and unusable addresses are left out ("Local Area Connection" is not
  mistaken for `lo`); the usual network and a tunnel are told apart (IPv4 or IPv6); saved names are checked; this
  computer is read without failing.
* `system-integration` `adapters` (5): Linux, Windows and macOS adapters named by what they are (a USB Wi-Fi is
  Wi-Fi; a phone by its driver or USB product name; a USB Ethernet with a standard driver is not a phone; bridges and
  virtual adapters unknown); this computer read without failing. UI: `networks.test.ts` (2: plain names, numbered
  when there are several of a kind, the system's name when unknown), `throttle.test.ts` (+1: held off).
* `tests/integration/tests/multinet.rs` (6, Linux and Windows): a download spread over two networks (two loopback
  addresses; the server sees both) is intact, the details and Settings say what came through each; a network that
  fails never fails a download (no retry spent); a server that refuses a network gets its requests the usual way; off,
  one network, a proxy and a VPN-like route spread nothing, and the choice is saved; bad choices are refused; Smart
  throttle is held off while it is on (the user's switch kept, cannot be turned on meanwhile, held after a restart,
  back when it is turned off).
* `tests/e2e/desktop-multinet.mjs` (21 checks, real app): the panel lists the networks by name (no address), the
  switch, a 12 MB download through both networks (server side), intact, "In use" and what each received, the details
  line, the report, leaving one out ("choose at least two"; nothing spread), refused input, off. Screenshot
  `docs/screenshots/multi-network-dark.png`.
* Stress: the integration tests 30 times and the unit tests 20 times with every core busy, no failure.
* Real networks (by hand, two or more connected networks and the internet): `multinet_real.rs` (`--ignored`: the
  usual route, then spread, whole file, every network carried part of it by the system's counters, at least 1.2× faster;
  and a per-network timeline every 2 s) and `tests/e2e/real-multinet.mjs` (the real app with the computer's networks:
  listed by plain name and one "usual", Smart throttle held off with its switch greyed out at once, a 145 MB download
  through every network, whole, Settings shows them in use, Smart throttle back when it is turned off: 15 checks);
  `switching_between_real_networks_when_one_goes_or_comes_back` (each network disconnected and connected again with
  `nmcli` during a 4 GB download: never stops, never fails, nothing through a disconnected network by the app's
  count, each one used again) and `show_one_network_going_and_coming_back` (`VELOX_TEST_UNPLUG=<device>`).
* Unit tests for each real-run fix: an unmeasured network gets its share at once; a network read again with another
  address keeps what was learned; one "usual" with IPv6 on the phone; a floor of one connection per network (the
  server's "too many" still wins); a lost transfer on one network is not counted against the server; a crawling
  transfer is told from a slower network; a network that stops sending is given up and rests; a network that goes
  away ends its transfers at once; a burst of failures counts once; a route from a link-local address reaches
  nothing; requests over several networks last a few seconds (512 KiB – 16 MiB); a reading of the system that hangs
  is not waited for and never piles threads up.
