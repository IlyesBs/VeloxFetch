# VeloxFetch

**High-performance, secure, cross-platform download manager** — a Rust download engine with a Tauri v2
desktop app (React + TypeScript + Tailwind).

[![License: GPL v3](https://img.shields.io/badge/License-GPLv3-blue.svg)](LICENSE)
![Platforms](https://img.shields.io/badge/platforms-Windows%20%7C%20Linux-informational)

VeloxFetch downloads files fast and reliably, integrates with your browser, handles video sites, and keeps
everything on your computer — no accounts, no telemetry. It runs on Windows and Linux, in eight languages, with a
right-to-left interface in Arabic.

## Download

Get the latest build for **Windows** and **Linux** from the releases page:
**https://github.com/IlyesBs/VeloxFetch-releases/releases** (macOS: in progress).

## Support

VeloxFetch is free and developed by one person. If it is useful to you, a small donation helps keep it going —
entirely optional, and the app is never limited for not donating.

* **Crypto:** [![Donate](https://img.shields.io/badge/Donate-Crypto-blue?style=for-the-badge)](https://link.depay.com/4VnYyZSV7h526UxFEVDIvO)
* **Card / Payoneer:** coming soon.

Thank you.

## Features

### Fast, reliable downloads
Each download uses up to **64 connections** (it starts on 4 and climbs 4 → 16 → 64 while that keeps making it
faster). On a slow server it is at least as fast as a browser. Lowering the connection count never cuts a transfer,
and a broken single-connection transfer continues where it stopped — only when the server proves it is the same
file. The engine moves roughly **1.1 – 1.8 GB/s on loopback** at any connection count, uses a few percent of one
core at gigabit speed, and finishes the file as the last byte lands.

### Never lose a download
A failure the network or server may recover from (no connection, DNS, timeout, dropped connection, 408 / 425 / 429 /
5xx) is retried on its own from the bytes already saved. Offline, nothing is sent and no attempt is wasted; it
starts when the network returns, and immediately after the machine wakes from sleep.

### Resume integrity
Finished parts are hashed as they are saved and re-checked before being trusted after a crash or an outside change.
Every ranged response must carry the download's version and total size, so two versions of a file are never
combined. Servers that give no strong version tag no longer force a resume to start over — a few small samples of
the saved bytes are compared with the server's first.

### Browser integration
Chrome, Edge and Brave get the extension from the **Chrome Web Store**; Firefox from
**[Firefox Add-ons](https://addons.mozilla.org/addon/veloxfetch/)**. Installing VeloxFetch asks the browser to add
it. A download started in the browser while VeloxFetch is closed launches the app — including for browsers installed
as Snap or Flatpak packages. The confirmation dialog can also take a site's username and password when the server
asks for them.

### Video downloads
With the extension installed, a **Download video** bar appears over videos — files, HLS / DASH streams, and video
pages on YouTube, Vimeo and similar sites. Choose the quality and the container (MP4 by default, MKV or WebM; MP3,
M4A or Opus for audio only). The helper programs video sites need are fetched and kept up to date automatically,
with nothing to install by hand. A video can be paused, resumed and watched while it downloads, is retried by itself
when the site or network fails part-way, and lands in the history. Videos protected with DRM cannot be downloaded,
and the app says so.

### Smart throttle
Three levels (Settings → Speed, and the dashboard card):
- **Fast** — full speed, never slowed.
- **Smart** — downloads give way on their own to a video, a game, a call or browsing, and take the whole connection
  back when the machine is idle; smooth, and never below a reserved minimum.
- **Manual** — the level you pick (Aggressive, Balanced, Performance), all the time.

Under a cap a download keeps only the connections it needs and reads in small, even steps, so the line sees no
bursts and nothing stops when a cap starts, changes or lifts. The detail view can **preview** audio and video while
they download.

### Smart download queue and scheduling
Create several queues, each with a few plain rules (time windows, idle-only, not on mobile data, pause while the
connection is busy, big files here…). Give each download High / Normal / Low priority, start it now, later or at a
set time, reorder it, or move it between queues. Each row shows what it is doing and why — Downloading, Waiting for
a free slot, Scheduled, Paused, Completed, Failed or Retrying — with a "Next download" line and a 24-hour timeline.

### Before a download starts
A pre-download check (for links from the browser or clipboard, and the Add dialog's **Check link**) shows the file,
its size and type, whether it can be resumed and how it will start, whether it fits on the drive, whether the
connection is secure, whether you already have it, and any warnings (sign-in needed, temporary or expired link, a web
page instead of a file, not enough space). It downloads nothing, writes nothing, can be cancelled, and never blocks
you from downloading anyway.

### More reliability tools
- **Use a new link** — continue a paused or failed download from a fresh link; saved bytes are kept only when the
  new link is proven to be the same file.
- **Download again** — refetch a finished, cancelled or failed download from its own entry; an existing file is
  never overwritten.
- **Smart duplicate detection** — adding a link the app already has tells you its state and offers what fits
  (view, resume, open, or download another copy); a mere name clash is not treated as a duplicate.
- **Download doctor** — when a download fails, the detail view explains what happened, where the problem is, whether
  retrying helps, and what to do.
- **Download profiles** — Default, Fast, Balanced, Low resource, and your own; presets over speed limit, downloads
  at once, connections and Smart throttle, applied without restarting downloads.

### Privacy and security
Everything stays on your computer: no accounts, **no telemetry**, no remote code. A small rotating diagnostics log
(Settings → Diagnostics) has passwords, cookies, tokens and link query values removed before anything is written.
Secrets (a site's cookies or auth header for a paused download) are sealed with a key kept only in the operating
system's credential store, never in a file.

### Secure in-app updates
Settings → Updates (and the status bar) tells you when a newer version exists, downloads it on request and installs
it (the Windows setup, an AppImage that replaces itself, or the system installer for `.deb` / `.rpm` / `.dmg`). Only
an update signed with the publisher's offline Ed25519 key is accepted, it is never older than what you have, and its
exact size and checksum are verified before it is kept and again before it is installed.

### On the desktop
Closing the window keeps VeloxFetch in the tray; it can start at login and notifies you when a download finishes or
fails while the window is hidden.

## Build from source

**Requirements:** Rust (stable), Node 20+, and — on Windows — the MSVC build tools, the Windows SDK and the WebView2
runtime.

```bash
# Engine + manager tests (fast, no UI)
cargo test --workspace

# Command-line client
cargo run -p velox-cli -- get https://example.com/big.iso --db state.db
cargo run -p velox-cli -- list --db state.db     # pause = Ctrl-C once, cancel = twice

# Desktop app (development, hot reload)
cd apps/desktop && npm install && npm run tauri dev

# Desktop app (release build)
cd apps/desktop && npm run tauri build

# Frontend unit tests
npm --prefix apps/desktop test
```

End-to-end tests that drive the real desktop app live in `tests/e2e/`.

## Project layout

```
apps/desktop             Tauri v2 app: src/ (React UI), src-tauri/ (commands, capabilities, CSP)
apps/cli                 `velox` command-line client
apps/native-host         `velox-native-host`: the program browsers start for the extension
apps/browser-extension   Manifest V3 extension (Chromium + Firefox), no dependencies
crates/http-client       reqwest wrapper: probing, verified ranges, pooling, DNS cache
crates/downloader-core   the engine (chunking, retry, resume, limits, scheduling, integrity)
crates/persistence       SQLite state + crash recovery
crates/scheduler         time-window scheduling (start/stop/days), pure logic
crates/system-integration  power actions behind a trait; native-messaging host registration
crates/browser-bridge    browser ⇄ app protocol, validation, authentication, local link
crates/download-manager  application controller used by the UI
crates/diagnostics       local log (redacted, rotated) and text reports; no telemetry
crates/update-verify     checks an update before anything is written or run (Ed25519, version, hash)
```

## License

VeloxFetch is free software, licensed under the **GNU General Public License v3.0 or later** ([`LICENSE`](LICENSE)).
You may use, study, share and modify it; a distributed modified version must stay under the GPL with its source
available. There is no warranty, to the extent permitted by law.
