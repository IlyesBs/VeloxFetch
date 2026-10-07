# VeloxFetch

High-performance, secure, cross-platform download manager: a Rust download engine with a Tauri v2
desktop shell (React + TypeScript + Tailwind).

**New here? Read [`VELOXFETCH-OVERVIEW.md`](VELOXFETCH-OVERVIEW.md) first**: the whole app (features, design,
security, roadmap) on one page. Details: [`docs/architecture.md`](docs/architecture.md) and [`docs/security.md`](docs/security.md).

## Layout

```
apps/desktop        Tauri v2 app: src/ (React UI), src-tauri/ (commands, capabilities, CSP)
apps/cli            `velox` command-line client
apps/native-host    `velox-native-host`: the program browsers start for the extension
apps/browser-extension  Manifest V3 extension (Chromium + Firefox), no dependencies
crates/http-client  reqwest wrapper: probing, verified ranges, pooling, DNS cache
crates/downloader-core   the engine (chunking, retry, resume, limits, scheduling, integrity)
crates/persistence  SQLite state + crash recovery
crates/scheduler    time-window scheduling (start/stop/days), pure logic
crates/system-integration  power actions behind a trait; native messaging host registration
crates/browser-bridge  browser <-> app protocol, validation, authentication, local link
crates/download-manager  application controller used by the UI
crates/diagnostics  local log (redacted, rotated) and the text reports; no telemetry
tests/integration   fault-injecting local server + end-to-end layer tests
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
node tests/e2e/desktop-browser.mjs # real native host process <-> real app (hostile input, auth, limits)
node tests/e2e/desktop-folders.mjs    # download folders, sorting by type
node tests/e2e/desktop-usability.mjs  # icons, open / show in folder, search, sign-in, network, shortcuts, paste / drop
node tests/e2e/desktop-diagnostics.mjs  # diagnostics card, Settings > Diagnostics, log file, no secret anywhere
node tests/e2e/desktop-video.mjs  # video bar hand-over -> video dialog -> file (stand-in yt-dlp), DRM refusals, 64 connections
node tests/e2e/desktop-preview.mjs  # live preview through the velox-preview protocol, smart throttle with the real process scan
node tests/e2e/real-browser.mjs  # the built extension in a real Brave (skips if none is installed)
node tests/e2e/real-youtube-ui.mjs target/debug/velox-desktop.exe <tools folder>  # by hand, internet: a real YouTube video in the app window (dashboard, Watch now, Pause / Resume, History)
```

Videos: with the browser extension installed, a **Download video** bar appears over videos
(files, HLS / DASH streams, and video pages on YouTube, Vimeo and similar sites); click it and choose the quality and the file type
(MP4 by default, MKV or WebM; MP3, M4A or Opus for only the sound). The
programs video sites need (yt-dlp, Deno, and ffmpeg for HD) are downloaded by the installer itself (Windows setup, Linux
deb / rpm; on macOS and if the installer had no internet, by VeloxFetch at its first start, continuing where it stopped if it is
closed in the middle) and kept up to date: nothing to install (Settings > Browser & clipboard > Video tools shows them).
A video downloading in the Videos list can be paused and resumed, watched while it downloads, and is tried again by itself
when the site or the network fails part-way (Retry continues from what is already there). Videos count in the dashboard
(speed, active, network) and land in the History. Videos protected with DRM (Netflix, Shahid, STC tv...) cannot be
downloaded, and VeloxFetch says so.

Smart throttle (Settings > Speed) lets downloads give way while you browse, watch or play (automatic mode, or while a
listed game / call runs) and gives them the whole connection when the network or the computer is idle, smoothly and never
below the reserved minimum; a status line and a speed graph show what it does. The details of a video or audio download
can **preview** it while it downloads. See [`docs/smart-throttle-and-preview.md`](docs/smart-throttle-and-preview.md).

On the desktop: closing the window keeps VeloxFetch in the tray, it can start at login (in the tray), and it notifies when
a download finishes or fails while the window is hidden (Settings > Appearance > On the desktop). A download sent from the
browser while VeloxFetch is closed starts it, also for browsers installed as snap / Flatpak packages.

Speed: each download uses up to 64 connections (it starts on 4 and climbs 4 -> 16 -> 64 while that makes it faster).

Diagnostics stay on the computer: the app writes a small rotating log to `<app data>/logs/velox.log`
(Settings > Diagnostics > Open log folder) with passwords, cookies, tokens and link query values removed.

The UI types in `apps/desktop/src/types.ts` mirror the DTOs in
`crates/download-manager/src/api.rs`; keep them in sync.
