# VeloxFetch

High-performance, secure, cross-platform download manager: a Rust download engine with a Tauri v2
desktop shell (React + TypeScript + Tailwind).

**Version 1.0.0.** Release gate and steps: [`docs/release-candidate.md`](docs/release-candidate.md). Publishing to
users (one command, the public repository `IlyesBs/VeloxFetch-releases`, texts and screenshots):
[`docs/publishing.md`](docs/publishing.md). The public page names only the systems that have been built (macOS
appears once it is).

**Interface:** every field, drop-downs included, is drawn in the theme's colours on Windows, Linux and macOS
([`docs/ui-performance.md`](docs/ui-performance.md)).

**Arabic:** right-to-left with no stray sideways scrollbar on Linux ([`docs/ui-performance.md`](docs/ui-performance.md)).

**Lists:** percentages have one decimal everywhere, and a download that has not started yet is named by its link
([`docs/smart-download-queue.md`](docs/smart-download-queue.md), [`docs/smart-duplicate-detection.md`](docs/smart-duplicate-detection.md)).

**New here? Read [`VELOXFETCH-OVERVIEW.md`](VELOXFETCH-OVERVIEW.md) first**: the whole app (features, design,
security, roadmap) on one page. Details: [`docs/architecture.md`](docs/architecture.md) and [`docs/security.md`](docs/security.md).

## Layout

```
apps/desktop        Tauri v2 app: src/ (React UI), src-tauri/ (commands, capabilities, CSP)
apps/cli            `velox` command-line client
apps/native-host    `velox-native-host`: the program browsers start for the extension
apps/browser-extension  Manifest V3 extension (Chromium + Firefox 140+), no dependencies
crates/http-client  reqwest wrapper: probing, verified ranges, pooling, DNS cache
crates/downloader-core   the engine (chunking, retry, resume, limits, scheduling, integrity)
crates/persistence  SQLite state + crash recovery
crates/scheduler    time-window scheduling (start/stop/days), pure logic
crates/system-integration  power actions behind a trait; native messaging host registration
crates/browser-bridge  browser <-> app protocol, validation, authentication, local link
crates/download-manager  application controller used by the UI
crates/diagnostics  local log (redacted, rotated) and the text reports; no telemetry
crates/update-verify  checks an update before anything is written or run (Ed25519 manifest, version, hash)
tests/integration   fault-injecting local server + end-to-end layer tests
tests/bench         `velox-bench`: the performance benchmark (not shipped)
tests/e2e           drives the real desktop app through WebView2
```

## Requirements

Rust (stable), Node 20+, and on Windows the MSVC build tools + Windows SDK and the WebView2 runtime.

## Everyday commands

```bash
# engine + manager tests (fast, no UI)
cargo test --workspace

# CLI
cargo run -p velox-cli -- get https://example.com/big.iso --db state.db
cargo run -p velox-cli -- list --db state.db     # pause = Ctrl-C once, cancel = twice
VELOX_LOG=debug cargo run -p velox-cli -- get <url>   # the engine's log on stderr (secrets removed)

# desktop app (dev, hot reload)
cd apps/desktop && npm install && npm run tauri dev

# desktop app (release build)
cd apps/desktop && npm run tauri build

# frontend unit tests (store, formatting)
npm --prefix apps/desktop test

# end-to-end tests of the real app (Windows; debug build with the bundled UI)
npm --prefix apps/desktop run build
cargo build -p velox-desktop -p velox-cli -p velox-native-host --features velox-desktop/custom-protocol
node tests/e2e/desktop-ipc.mjs   # IPC, security boundaries, shutdown/restart
node tests/e2e/desktop-ui.mjs    # every view, live updates, 60 FPS, CSP; writes docs/screenshots
node tests/e2e/desktop-queue.mjs # queue limits, pause/resume, scheduler, power action (dry run), restart
node tests/e2e/desktop-smart-queue.mjs # several queues, rules, start at, order, timeline, Add dialog choices, hostile IPC
node tests/e2e/desktop-browser.mjs # real native host process <-> real app (hostile input, auth, limits)
node tests/e2e/desktop-folders.mjs    # download folders, sorting by type
node tests/e2e/desktop-card-flash.mjs # starting downloads never makes the cards flash (issue #20)
node tests/e2e/desktop-usability.mjs  # icons, open / show in folder, search, sign-in, network, shortcuts, paste / drop
node tests/e2e/desktop-diagnostics.mjs  # diagnostics card, Settings > Diagnostics, log file, no secret anywhere
node tests/e2e/desktop-video.mjs  # video bar hand-over -> video dialog -> file (stand-in yt-dlp), DRM refusals, 64 connections
node tests/e2e/desktop-preview.mjs  # live preview through the velox-preview protocol, smart throttle with the real process scan
node tests/e2e/desktop-throttle.mjs # Smart throttle card / Settings / status bar: clicks, modes, a capped download that never stops
node tests/e2e/desktop-gaming.mjs   # gaming policy with a stand-in game process: pause / resume only what it paused, low speed, off
node tests/e2e/desktop-game-mode.mjs  # Game mode in one click: status bar, banner, Ctrl+G, Settings, pause policy, restart, 8 languages fit
node tests/e2e/desktop-perf.mjs     # start-up time and frame rate with downloads running (see docs/ui-performance.md)
node tests/e2e/real-browser.mjs  # the built extension in a real Brave (skips if none is installed)
node tests/e2e/desktop-extension-wait.mjs  # first start with the store extension: nothing opens, the wait, the fallback banner, "installed and connected" on its first contact, AppImage opens the store page
VELOX_REAL_BROWSER=chrome node tests/e2e/real-store-extension.mjs  # by hand, internet, Linux + Chrome / Edge / Brave: the browser adds the Chrome Web Store extension by itself through the packages' pointer, the first start opens nothing, the extension reaches the app
node tests/e2e/real-youtube-ui.mjs target/debug/velox-desktop.exe <tools folder>  # by hand, internet: a real YouTube video in the app window (dashboard, Watch now, Pause / Resume, History)
node tests/e2e/real-throttle-switch.mjs target/debug/velox-desktop.exe  # by hand, internet: Fast / Smart / Manual switched every few seconds with the sidebar's buttons on a real download; the longest stop per step
node tests/e2e/real-game-mode.mjs target/debug/velox-desktop.exe  # by hand, internet: a kernel.org download held by Game mode (give way, low speed, pause) and given its speed back
node tests/e2e/real-publisher-checksum.mjs target/debug/velox-desktop.exe  # by hand, internet: the Linux kernel and a Debian image verified with their publisher's checksum
node tests/e2e/real-network-usage.mjs target/debug/velox-desktop.exe  # by hand, internet: a real download counted in Network usage, kept after a restart and deleting everything
```

Browser extension: Chrome, Edge and Brave get it from the **Chrome Web Store**. Installing VeloxFetch (Windows setup,
deb / rpm) asks them to add it and the browser does it by itself; VeloxFetch says **Browser extension installed** when
it first reaches the app, and offers the store page only if it has not come after 3 minutes. Settings > Browser shows
the Chrome Web Store badge and never asks for Developer mode (Firefox: a temporary add-on until its add-on is published).
See `docs/store-extension.md`.

Videos: with the browser extension installed, a **Download video** bar appears over videos
(files, HLS / DASH streams, and video pages on YouTube, Vimeo and similar sites); click it and choose the quality and the file type
(MP4 by default, MKV or WebM; MP3, M4A or Opus for only the sound). The
programs video sites need (yt-dlp, Deno, and ffmpeg for HD) are downloaded by the installer itself (Windows setup, Linux
deb / rpm; on macOS and if the installer had no internet, by VeloxFetch at its first start, continuing where it stopped if it is
closed in the middle) and kept up to date: nothing to install (Settings > Browser & clipboard > Video tools shows them).
A video downloading in the Videos list can be paused and resumed, watched while it downloads, and is tried again by itself
when the site or the network fails part-way (Retry continues from what is already there). Videos count in the dashboard
(speed, active, network) and land in the History. Files whose publisher lists a checksum next to them (SHA256SUMS…) are verified by themselves before they are
finished, and marked "Verified" (`docs/publisher-checksum.md`). Game mode: one click on the status bar's gamepad, the tray menu or
Ctrl+G and downloads give way to a game or a call at once, with a banner and a countdown (`docs/game-mode.md`). The Network usage card keeps everything VeloxFetch has received,
with today and this month, across restarts, updates, reinstalls and deleted downloads (`docs/network-usage.md`). Videos protected with DRM (Netflix, Shahid, STC tv...) cannot be
downloaded, and VeloxFetch says so.

Smart throttle (Settings > Speed, and the dashboard card) has three levels. **Fast**: full speed, never slowed.
**Smart**: downloads give way by themselves to a video, a game or a call, or browsing (a short check now and then finds
a video that full-speed downloads would starve; a listed game / call: give way, a low speed limit or pause, lifted when
it closes). **Manual**: the level you pick (Aggressive, Balanced, Performance), all the time. Smart gives downloads the
whole connection when the network or the computer is idle, smoothly and never
below the reserved minimum; a status line and a speed graph show what it does. Under a cap a download keeps only the
connections it needs and reads in small, even steps, so the line sees no bursts (what a game needs), and it never
stops when a cap starts, changes or lifts. The details of a video or audio download
can **preview** it while it downloads (on Linux and macOS through a private local address; the Linux packages
recommend the media plugins it needs, so a system that cannot install a codec still installs the app; the AppImage
carries only software playback plugins, never the machine's hardware ones). See [`docs/smart-throttle-and-preview.md`](docs/smart-throttle-and-preview.md).

On the desktop: closing the window keeps VeloxFetch in the tray, it can start at login (in the tray), and it notifies when
a download finishes or fails while the window is hidden (Settings > Appearance > On the desktop). A download sent from the
browser while VeloxFetch is closed starts it, also for browsers installed as snap / Flatpak packages.

Smart Download Queue (the Queue page): make several queues, each with a few plain rules. It is easiest to start
from a ready-made setup:
- **Night downloads:** 23:00 to 07:00, and big files go there.
- **When I'm away:** only while the computer is idle.
- **Start at 2:00.**
- **Light on the connection:** not on mobile data, and pause while the connection is busy.

Give each download High / Normal / Low priority. Start it now, later, or at a set time, move it up or down, or move it
to another queue. Each row says what the download is doing and why: Downloading, Waiting for a free slot, Scheduled
for 23:00, Paused, Completed, Failed or Retrying. A "Next download" line and a 24-hour timeline show what comes when.

Without rules nothing changes, and something the computer cannot tell never holds a download back. See
[`docs/smart-download-queue.md`](docs/smart-download-queue.md).

Download Doctor: when a download fails, its detail view says what happened, where the problem is (the server, your
connection, this computer, the link or the sign-in), whether retrying helps (now, later, after you fix something, or
not at all), and what to do. While the app retries on its own, it shows what it retries and when. See
[`docs/download-doctor.md`](docs/download-doctor.md).

The confirmation for a download from the browser can also take the site's user name and password (its "Sign in to
this site" part opens by itself when the server asks for them).

Download Intelligence: before a download starts, the confirmation dialog (links from the browser or the clipboard)
and the Add dialog's **Check link** show:
- the file, its size and type;
- whether it can be resumed, and how it will start;
- whether it fits on the drive, and whether the connection is secure;
- whether you already have it;
- any warnings: sign-in needed, temporary or expired link, a web page instead of the file, not enough space.

"Why?" shows what the server actually did. The check downloads nothing, writes nothing and can be cancelled. If it
fails, you can still download. See [`docs/download-intelligence.md`](docs/download-intelligence.md).

**Download Profiles**: Default (your settings), Fast, Balanced, Low resource, and profiles of your own. They are
presets over the speed limit, the downloads at once, the connections and Smart throttle's automatic mode. Switching
applies at once without restarting downloads, keeps their progress and never undoes a pause. See
[`docs/download-profiles.md`](docs/download-profiles.md).

**Video download details**: clicking a video download opens a details page like a file's. It shows progress,
speed (live chart, average, peak), time, size, pieces, the player, quality, file type, link, folder and saved files,
with every action. See [`docs/video-details.md`](docs/video-details.md).

**Download again**: a finished, cancelled or failed download can be fetched once more from the start, from its own
entry (same link, folder and name). A finished file deleted by mistake shows as missing, and "Download again"
becomes its main button. A file that is still there is never overwritten. See [`docs/redownload.md`](docs/redownload.md).

**Languages**: English, French, Arabic, Spanish, Portuguese (Brazil), German, Russian and Turkish. VeloxFetch starts in your computer's language. See
[`docs/languages.md`](docs/languages.md).

**Several networks at once**: turn it on in Settings > Network and a download's connections are spread over your
networks (Wi-Fi, a cable, a phone shared over USB), so their speeds add up. Only for secure links to the internet,
never through a proxy or around a VPN; leave out any network (a phone with a data limit). A network that stops
working hands its part back to your usual connection at once. While it is on, the dashboard says how many networks
are used and shows each network's speed and their total. See [`docs/several-networks.md`](docs/several-networks.md).

**Never lose a download**: a download that failed because of the network or a server's passing trouble is tried
again on its own, from its saved bytes; offline it waits for the connection, and after the computer sleeps it is tried
at once. See [`docs/never-lose-a-download.md`](docs/never-lose-a-download.md).

**Resume integrity**: finished parts of a download are hashed as they are saved and checked again before being
trusted after a crash or an outside change. Every ranged answer must carry the download's version and total size,
uncompressed, so two versions of a file are never combined. Servers that give no strong version tag (most of them)
no longer make a resume start over: a few small pieces of the saved bytes are compared with the server's first.
See [`docs/resume-integrity.md`](docs/resume-integrity.md).

**Use a new link**: a paused or failed download (an expired signed link, an ended session, a mirror gone) continues
from a fresh link, pasted or re-clicked in the browser. The saved bytes are kept only when the new link is proven to
give the same file: same size and strong version tag, ranges that work, and sampled saved bytes that match. Another
file under the same name is refused and nothing is changed. Cookies never follow a link to another server. See
[`docs/new-link-recovery.md`](docs/new-link-recovery.md).

**Smart Duplicate Detection**: adding a link the app already has says so before anything happens. It tells you
whether that download is running, paused, failed or finished, and whether the finished file is still there. It
offers what fits: view it, resume it, open the file, or download another copy. A file that only has the same name is
not a duplicate: the new download gets its own name and nothing is ever replaced. See
[`docs/smart-duplicate-detection.md`](docs/smart-duplicate-detection.md).

Speed: each download uses up to 64 connections (it starts on 4 and climbs 4 -> 16 -> 64 while that makes it faster).
On a slow server it is at least as fast as a browser. Going down in connections never cuts a transfer, and a broken
one-connection transfer continues where it stopped (only when the server proves it is the same file). See
[`docs/slow-servers.md`](docs/slow-servers.md).

**Performance** (`docs/benchmarks.md`): the engine moves 1.1 – 1.8 GB/s on loopback whatever the connection count,
uses 1.4 – 3 % of one core at gigabit speed, and finishes the file as the last byte lands. Measure it yourself with
`cargo build --release -p velox-bench && ./target/release/velox-bench run --sizes 1G --conns 1,16,64`.

**Updates**: Settings > Updates (and the status bar) tells you when a newer version exists, downloads it when you ask
and installs it (the Windows setup, an AppImage that replaces itself, the system's installer for `.deb` / `.rpm` /
`.dmg`). Only an update signed with the publisher's offline key is accepted; it is never older than what you have,
and its exact size and checksum are checked before it is kept and again before it is installed. Releases are signed
with `node scripts/update-key.mjs`. See [`docs/auto-update.md`](docs/auto-update.md).

Diagnostics stay on the computer: the app writes a small rotating log to `<app data>/logs/velox.log`
(Settings > Diagnostics > Open log folder) with passwords, cookies, tokens and link query values removed.

The UI types in `apps/desktop/src/types.ts` mirror the DTOs in
`crates/download-manager/src/api.rs`; keep them in sync.
