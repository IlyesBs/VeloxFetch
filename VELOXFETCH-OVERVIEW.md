# VeloxFetch: the whole app on one page

> Read this first. It explains what VeloxFetch is, what it does, how it is built, how it is kept safe, where the
> project stands (Phases 0 to 14 done) and what comes next (Phases 15 to 22). It does not require reading the
> code. Details live in `docs/` (links at the end of each section).

---

## 1. What it is

VeloxFetch is a **fast, secure download manager for Windows, Linux and macOS**, in the spirit of IDM:

- It splits every file into pieces and downloads them over up to **64 connections** at once.
- A download survives pauses, crashes and network cuts.
- It takes downloads from the browser (Chrome, Edge, Brave, Firefox) and also saves videos (YouTube-like
  sites, HLS / DASH streams).

**Built with:**

| Part | Technology |
|---|---|
| Engine | Rust, the core and the whole product |
| Desktop shell | Tauri v2 |
| User interface | React 19 + TypeScript + Tailwind v4 |
| Local data | SQLite |

**Rule number one: the UI is not the application.** The product is the Rust download core. The desktop window
is just one client of it; the CLI and the tests are others. The UI can be replaced without touching the engine.

**Version:** 0.1.0 (pre-release). There is no git repo. Named backups are kept in `C:/work/VeloxFetch-backups/`.

---

## 2. What a user can do (features)

### Downloading
- **Ways to add a download:**
  - paste a link (Ctrl+V anywhere), drop it on the window, or type it in the link bar;
  - **Add several**: a list of links, or a pattern such as `file_[001-100].zip`;
  - from the browser: a right-click, or automatic take-over.
- **Speed:**
  - up to 64 parallel connections per file, adapted live (see §4.2);
  - verified range support: servers that do not support it get one connection.
- **Pause / Resume / Cancel / Retry:**
  - Pause keeps everything; Cancel deletes the partial file;
  - Resume first checks that the file on the server did not change (ETag, size). If it changed, the download restarts.
- **Crash-safe:** the progress is saved every 500 ms, in a safe order. After a crash or a power cut, the
  download continues from the bytes that really reached the disk.
- **Integrity:** an optional SHA-256 / SHA-512 checksum is checked at the end.
- **Speed limits:** global, per kind of file (video, ISO…) and per download, all changeable while running.
- **Clear errors:** each failure gets a plain sentence plus "what you can do" (sign in, free space, choose a folder).
- **Sign in:** sites that need a login (HTTP Basic, cookies from the browser) are handled.

### Files and folders
- **Folders:**
  - a default download folder, changeable;
  - a folder per download, chosen before it starts;
  - optional **sorting by type** (Video, Music, Programs…).
- **Names:**
  - a file is never overwritten: the next free name is used (`name (1)`);
  - file names coming from servers are made safe.
- **Finished files:**
  - Open, Show in folder, Remove from list (the file stays), **Delete from disk**;
  - Delete always asks first, and Cancel is the default answer;
  - Open refuses programs (`.exe`…): only Show in folder.
- **History:** every finished file. Search it, open it, download it again, or remove it.

### Queue, schedule, power
- **Queue:**
  - set the maximum downloads at once and the maximum connections per download;
  - priorities run from Lowest to Highest;
  - pause the queue (optionally also pausing the running downloads) and resume it.
- **Smart Download Queue** (details in `docs/smart-download-queue.md`):
  - **Several queues**, one tab each, with ready-made setups: night downloads, when I'm away, start at 2:00, light
    on the connection.
  - **Rules per queue:**
    - when: anytime, between two times, or starting at a time until it is done;
    - only while the computer is idle;
    - Wi-Fi only / cable only / not on mobile data;
    - pause while the connection is busy;
    - big files move to it.
  - **Per download:** High / Normal / Low; start now, later, or at a set time; move up / down; move to another queue.
  - **Queue actions:** start (whatever the schedule, until empty), pause, edit schedule, clear completed, priority
    for all, reorder queues.
  - **Clear states:** Downloading, Waiting (with why), Scheduled, Paused, Completed, Failed, Retrying. Plus a "Next
    download" line, and a 24-hour timeline when times are involved.
  - **Smart on its own:**
    - nothing new starts without a network;
    - Low priority waits while Smart Throttle slows downloads;
    - a High download never waits only behind less urgent ones (one extra slot);
    - what the computer cannot tell never holds a download back.
  - **No change without rules:** the main queue has no rule until the user gives it one, so nothing changes for
    someone who never sets one.
- **Download Intelligence** (details in `docs/download-intelligence.md`): before a download starts, the browser
  confirmation dialog and the Add dialog's **Check link** show:
  - file, type, size, and resume (supported / likely / not supported / unknown);
  - how it will start (parallel with N connections, one connection, careful, or the video dialog);
  - whether it fits on the drive (including the 4 GB limit of FAT32 drives);
  - HTTPS or HTTP, and whether the app already has it (same link, same final address, or same file under another link);
  - warnings: sign-in needed, temporary or expired link, a web page instead of the file, server errors.

  "Why?" lists what was observed. Everything comes from one quick look at the server: nothing is downloaded or
  written, it can be cancelled, and it never blocks the download.
- **Schedule:** e.g. "start at 22:00, stop at 07:00, on weekdays".
- **Power action:**
  - shut down, sleep or hibernate once *everything* has finished;
  - a visible countdown runs first and can be cancelled;
  - off by default, forgotten at restart, and it fires only once.

### Browser integration
- **Pieces:** an extension for Chromium browsers and Firefox talks to a small helper program
  (`velox-native-host`), which talks to the app over an authenticated local link.
- **What it does:**
  - takes over browser downloads, with a confirmation window before anything starts;
  - passes on the referrer, cookies and user agent so protected links work. They are kept sealed and deleted
    when the download ends;
  - shows a **"Download video" bar** over videos on pages;
  - **starts VeloxFetch when it is closed**: a download sent from the browser opens the app and arrives in it. The
    helper finds the app itself (beside it, the Linux packages' `bin`, the macOS bundle, or the path the app recorded
    for an AppImage); a browser in a snap / Flatpak asks the desktop to open `veloxfetch://start`.
- **Install:** a button in Settings > Browser registers the helper per user and opens the extension page. Browsers
  installed as snap / Flatpak packages get their own copy of the helper and link.

### Videos
- **Video pages** (YouTube, Vimeo… through yt-dlp):
  - the user picks a quality and a type: MP4 (default), MKV or WebM;
  - or the sound only: MP3, M4A or Opus.
- **Streams:**
  - HLS (`.m3u8`) and DASH (`.mpd`), with quality, audio and subtitles;
  - ffmpeg joins picture and sound without re-encoding.
- **Video tools:** yt-dlp, Deno and ffmpeg are fetched by the installer or by the app, verified, and kept up to date.
- **While a video downloads:**
  - pause / resume, **watch it while it downloads**, automatic retry;
  - the progress bar moves for real, including during conversion;
  - speed limits and the Smart throttle apply to videos too.
- **Refused, with a clear reason:** DRM-protected services (Netflix, Shahid…), encrypted streams and live streams.

### Smart throttle
- **What it does:** downloads give way to what the user is doing and take the whole connection when it is free.
  - **Automatic mode** (on by default once Smart throttle is on): when something else uses the internet while the
    user is at the computer (browsing, a video), downloads slow down; heavy traffic while the user is away slows them
    less; when the network is free or the computer idle (no input for 2 minutes), full speed.
  - **Listed programs:** while a game or call app runs (LoL, Valorant, CS2, Zoom, Teams…), downloads keep only a
    share of the speed; full speed again when it closes.
- **Steady, never stuck:** smoothed readings, separate on / off thresholds, two readings to confirm before slowing,
  a calm period before speeding up (longer on an unsteady network), then steps of 1.5x. Downloads never go below the
  reserved minimum, so they never stop.
- **Settings:** on / off, Automatic, the mode, "Downloads always keep at least" (default 256 KB/s), and the maximum
  download speed (the global speed limit).
- **Modes:**

  | Mode | While browsing | During a game or a call |
  |---|---|---|
  | Aggressive | ~35 % | 10 % |
  | Balanced | ~50 % | 25 % |
  | Conservative | ~75 % | 50 % |

- **Shows what it does:** a status line ("Full speed — network idle", "Reducing speed — active browsing detected",
  "Bandwidth optimized — speeding back up"…) on the dashboard card, in Settings and in the status bar, and a graph
  of the download speed with the current limit (Settings > Speed > Smart throttle).
- **Default and look:** off by default. While it is on, the accent colour turns amber.
- **Privacy:** only program *names*, the computer's *total* traffic and the *time since the last input* are read;
  nothing is stored or sent.

### On the desktop (tray, start at login, notifications)
- **Tray:** closing the window keeps VeloxFetch running in the tray (downloads go on, the browser can still send
  new ones). The tray menu opens the window, pauses / resumes everything, or quits. A desktop without tray icons
  (some Linux desktops) closes the app as before.
- **Start at login** (off by default): VeloxFetch starts quietly in the tray (Windows `Run` key, XDG autostart,
  macOS launch agent).
- **Notifications** when a download finishes or fails while the window is hidden or behind another one, in the
  app's language. Starting VeloxFetch again brings the hidden window back.
- Settings > Appearance > **On the desktop** holds the three switches.

### Live preview
- The details of an audio / video download can **play it while it downloads**, from the bytes already on disk.

### Clipboard (opt-in)
- When a copied text holds download links, the app offers to download them.

### Diagnostics (all local)
- Each download has a timeline: server answers, retries, connections, events.
- A local log rotates and has secrets removed. A text report can be copied. Detailed logging can be switched on.
- **No telemetry:** nothing is ever sent anywhere.

### Interface
- **Pages:**

  | Page | Content |
  |---|---|
  | **Downloads** | The dashboard: link bar, 6 stat cards, tabbed task cards, side column with live Network ↓↑ / Disk write-read, upcoming downloads, quick actions |
  | **Active** | Running and waiting work: live speed, connections in use, time left, Pause all / Resume all |
  | **Queue** | The queue page |
  | **Completed** | Finished files: search, kind filter, open / folder / remove / delete |
  | **History** | Every finished file |
  | **Settings** | General, Folders, Speed, Network (proxy, user agent), Schedule, Browser & clipboard & video tools, Shortcuts, Diagnostics, About |

- **Download detail:** a segment map (canvas), network chart, controls (priority, limit), metadata and preview.
- **Look:**
  - "Neon glass" design (reference: `docs/design/neon-dashboard.png`);
  - dark / light / system theme;
  - Windows 11 Mica, a custom title bar;
  - responsive from 640×480: the sidebar folds below 900 px.
- **Languages:** English, French and Arabic (full right-to-left layout).
- **Shortcuts:**

  | Keys | Action |
  |---|---|
  | Ctrl+V | Paste a link anywhere |
  | Ctrl+L | Go to the link bar |
  | Ctrl+N | Add a download with options |
  | Ctrl+Shift+N | Add several downloads |
  | Ctrl+F | Search downloads or history |
  | Ctrl+1 / 2 / 3 | Downloads / Queue / History |
  | Ctrl+, | Settings |

  On macOS, use ⌘ instead of Ctrl.

### Command line
- `velox get <url>`, `velox resume <id>`, `velox list`, `velox info <url>`: the same engine, without the UI.

---

## 3. How it is built (map)

```
 Browser extension ─► velox-native-host ─► authenticated local pipe/socket ─┐
                                                                            ▼
 React UI (apps/desktop/src) ── Tauri IPC (ids + typed DTOs only) ──► Tauri commands + per-window authorization
                                                                            │   (apps/desktop/src-tauri)
                                                                            ▼
                                   DownloadManager (crates/download-manager): the application controller
                                   validation, lifecycle, queue, schedule, power, events, metrics, videos
                                                │                    │
                       Download engine (downloader-core)      Media engine (media-engine: HLS / DASH / yt-dlp)
                       http-client · planner · adaptive ·              (separate subsystem)
                       limiter · retry · integrity · disk
                                                │
                       persistence (SQLite) · path-safety · secrets · scheduler · system-integration · diagnostics
```

| Folder | One-line role |
|---|---|
| `crates/http-client` | Hardened HTTP: probes the file, verifies range support, pooling, DNS cache, safe redirects |
| `crates/downloader-core` | **The engine**: segments, adaptive connections, retries, limits, resume, integrity, disk writer |
| `crates/persistence` | SQLite (WAL, migrations, schema v6): downloads, segments, queue places, settings, history, diagnostics |
| `crates/download-manager` | What the UI talks to: every action, validation, queue / schedule / power, events, metrics, videos, previews |
| `crates/media-engine` | HLS / DASH parsers, segment downloads, yt-dlp runner, ffmpeg (arguments as arrays, never a shell) |
| `crates/path-safety` | Safe file names, "is it really inside the download folder", no following of planted links |
| `crates/secrets` | Seals cookies / passwords (ChaCha20-Poly1305) with a key held in the OS credential store |
| `crates/browser-bridge` | Browser ↔ app protocol: framing, strict validation, mutual authentication |
| `crates/scheduler` | Time windows and the Smart Download Queue's rules (pure logic) |
| `crates/system-integration` | Power actions, clipboard, browser detection, native-host registration, connection type (read only) |
| `crates/diagnostics` | Local redacted log and reports |
| `crates/failpoint` | Crash-test hooks (compiled out of shipped builds) |
| `apps/desktop` | Tauri v2 app: `src/` the React UI, `src-tauri/` commands, capabilities and CSP |
| `apps/cli` | The `velox` CLI |
| `apps/native-host` | The helper program that browsers start |
| `apps/browser-extension` | Manifest V3 extension, no dependencies |
| `tests/integration` | Fault-injecting local servers and crash tests (real processes killed at exact points) |
| `tests/e2e` | Drives the real app window (WebView2) |
| `tests/security` | Repository policy checks (CSP, capabilities, artifacts) |

More: `docs/architecture.md`.

---

## 4. How the key parts work

### 4.1 Life of a download
`create → queued → downloading → verifying → completed`

- From `downloading`: pause ↔ resume, or cancel (the partial data is deleted).
- `failed → retry → queued`.
- The file is written to `name.part`, then renamed at the end.
- **Checkpoint order:** snapshot the counters, then fsync the `.part`, then commit to SQLite. The database never
  claims bytes that are not on disk.

### 4.2 Speed: adaptive connections (up to 64)
- **Segments:** the file is cut into ~4 segments per possible connection. A gate decides how many run at once.
- **The controller:**
  - starts at **4** connections, which is gentle with servers that limit connections;
  - climbs **4 → 16 → 64** while each step makes the download ≥5 % faster;
  - judges each step one second *after* taking it, once the new connections are up to speed, and goes back
    if the step did not help;
  - after that, probes upward every ~15 s, doubling.
- **Backing off:**
  - halves on errors, or when the speed collapses for 3 seconds in a row;
  - reacts **at once** to HTTP 429 / 503 (the server says "too many connections").
- **Speed limits:** when a limit holds the speed (≥80 % of it), the controller changes nothing. A capped speed
  says nothing about the network.
- **Limits are token buckets:**

  | Bucket | Set by |
  |---|---|
  | global | the user |
  | per category | the user |
  | per download | the user |
  | smart | the Smart throttle |

  The tightest one governs.
- **Measured:** on a real ~24 Mbit/s line, 64 connections, a stable 2.6 to 2.9 MiB/s (the line's maximum).

### 4.3 Live UI at 60 FPS
- The manager never forwards each network write. Every 200 ms it sends **one "tick"**: all progress plus the
  global metrics. It goes quiet when idle.
- The UI store applies events once per animation frame. Untouched cards do not re-render.
- **Network and Disk panels:**
  - they show real system counters (through `sysinfo`): the whole PC's download / upload, and the read / write
    of the download disk;
  - loopback, tunnels and capture drivers are skipped;
  - they refresh once a second, idle too.

### 4.4 Browser hand-off
- The download passes through several checks:
  1. the extension validates it;
  2. it goes through native messaging to `velox-native-host`, which validates again;
  3. it crosses the local pipe / socket (mutual HMAC authentication, a new token every run);
  4. the app validates a third time;
  5. a confirmation window opens;
  6. the download is created.
- Only two commands exist: `ping` and `download`.
- The host starts the app only for a download, never for a `ping`, and only a program it found itself (never one
  named in a message). Inside a snap / Flatpak it asks the XDG desktop portal to open `veloxfetch://start` instead.

### 4.5 Videos
- **Analysis:** a link is analysed (qualities, audio, subtitles). Nothing is written at this step.
- **Ids only:** the UI sends back **ids only**. Rust re-reads the source and refuses unknown ids.
- **Work folder:** the work happens in a scratch folder. Then join / convert, then the final file.
- **Running video pages:**
  - at most 3 at once;
  - yt-dlp gets `--limit-rate`, and is restarted when the limit changes by more than 25 %;
  - it continues its partial files.

### 4.6 Smart Download Queue
- **One slot scheduler for everything.** It hands slots out by priority, then the queue order, then the user's
  place in the queue. Downloads whose queue is closed keep waiting while the next one goes.
- **Evaluation:** every second (`queues.rs`), each queue's rules are judged (pure logic in
  `scheduler::queue_rules`) on:
  - the time;
  - the idle time (from `SystemIntegration`, as for Smart Throttle);
  - the connection (routing table and adapter, nothing sent);
  - the other traffic.
- **A queue that closes** pauses its running downloads and remembers them; they resume when it opens.
- **When a start time comes**, that download starts.
- **Big files** move to the queue made for them.
- **New runs:** a download that starts is held back at once if its queue is closed, so it can never take a free
  slot first.
- **Database:** v6 adds the download's queue, place and start time to the `queue` table.

### 4.7 Download Intelligence
- **Gathering** (`manager/intelligence.rs`), read-only:
  - the download's own first request (`probe_quick`: `HEAD` + `Range: bytes=0-0`, no retry, 20 s cap);
  - the destination drive's free space (and, for files ≥ 4 GiB, its file system);
  - the downloads and history already known.
- **Rules** (`intelligence.rs`, pure): kind, resume (4 levels, a validator required for "supported"), strategy
  (`Downloader::connection_plan`, the engine's own rule), sign-in, temporary links, storage, duplicates (never by
  name alone), findings and a health label.
- **Where it shows:** `PendingDto.analysis` (confirmation dialog), and the `analyze_link` command (Add dialog, main
  window only).
- **Cancelling:** answering the dialog, a new check, or `url: null` cancels the check.
- **Secrets:** the shown link hides signatures and tokens; the result carries the final host only, error codes
  only, and is never logged.

---

## 5. Security model (always true)

**Mindset:** assume every external input is hostile. That covers URLs, HTTP answers, file names, headers, browser
messages, downloaded files, update data and UI requests.

- **WebView:**
  - strict CSP: no inline scripts or styles in HTML, no remote origins, no `eval`;
  - the page cannot make network requests.
- **IPC:**
  - ~60 fixed commands, each its own permission;
  - **no generic** "run / read any file" command;
  - inputs are typed, and unknown fields are refused;
  - a per-window authorization in Rust mirrors the capability files (a test keeps them identical).
- **The UI never sees or sends paths.** It works with ids and tokens; Rust resolves the paths.
- **Files:**
  - writes stay inside allowed folders, with links and junctions resolved;
  - reserved names, `..` and absolute paths are refused;
  - nothing is overwritten.
- **Secrets:**
  - cookies, passwords and the proxy password are sealed with a key held in the OS store;
  - there is no plaintext fallback, and they never appear in logs.
- **Processes:**
  - ffmpeg, yt-dlp and the power tools run with fixed arguments as arrays;
  - no shell, and nothing user-typed reaches a command line.
- **Power actions:** never fire on quit, and are forgotten at restart.
- **Updates:** no in-app updater yet, on purpose. A signed design comes first (Phase 19).
- **Release gate:** `scripts/security-gate.mjs` runs cargo-audit / deny, npm audit, a policy test, an SBOM and
  checksums.

More: `docs/security.md`, `docs/threat-model.md`, `docs/PRIVACY.md`.

---

## 6. Where data lives

| What | Where |
|---|---|
| Downloads, segments, settings, history, diagnostics | SQLite file in the app data folder |
| Smart Download Queue | same file: each download's queue, place and start time in the `queue` table; the queues and their rules in the settings (`queues.list`, `queues.state`, `queues.held`) |
| Sealing key for secrets | OS credential store (Windows Credential Manager / macOS Keychain / Secret Service) |
| Log | `<app data>/logs/velox.log` (rotating, redacted) |
| Video tools | `<app data>/tools` |
| UI-only preferences (folded sidebar, density, sort, open queue tab, language, theme) | `localStorage` |
| How the browser helper starts the app | `<app data>/app-path` (written by the app at each start) |
| Start at login (when turned on) | Windows `HKCU\...\CurrentVersion\Run`, `~/.config/autostart/app.velox.fetch.desktop`, `~/Library/LaunchAgents/app.velox.fetch.plist` |
| Second-launch signal (Linux / macOS) | `<app data>/show.sock` (or the user's runtime folder when the path is too long) |

Nothing is sent to any server except the downloads themselves.

---

## 7. Build, test, release

```bash
cargo test --workspace                    # engine, manager, integration and crash tests
npm --prefix apps/desktop test            # UI unit tests (vitest)
cd apps/desktop && npm run tauri dev      # run the app (dev)

# end-to-end on the real app (Windows):
npm --prefix apps/desktop run build
cargo build -p velox-desktop --features velox-desktop/custom-protocol -p velox-native-host -p velox-integration-tests
node tests/e2e/desktop-<suite>.mjs
#   suites: ipc, ui, redesign, views, usability, queue, smart-queue, folders, batch, media, video, preview, diagnostics, browser

# full release for the current OS (installer, portable zip, extension zips, SHA256SUMS, security gate):
bash scripts/build-release.sh
```

**Release outputs:**

| System | Files |
|---|---|
| Windows | NSIS `setup.exe` + portable zip |
| Linux | deb, rpm, AppImage |
| macOS | .app and .dmg |
| All | 3 extension zips (Chromium, Chrome Store, Firefox) and `SHA256SUMS.txt` |

**Signing:** not done yet, because it needs the owner's certificates.

More: `docs/testing.md`, `docs/release-process.md`.

---

## 8. Roadmap

**Legend:** ✅ done · 🔜 next · ⏳ later

| Phase | Topic | Status | In short |
|---|---|---|---|
| 0 | Architecture, security and design foundation | ✅ | Workspace, threat model, layers, "UI is not the app" |
| 1 | Rust download engine | ✅ | HTTP probing, ranges, segments, disk writer, CLI |
| 2 | Reliability and persistence | ✅ | SQLite, pause / resume, crash recovery, retries, integrity |
| 3 | Performance engine | ✅ | Adaptive connections (to 64), token-bucket limits, priority slots |
| 4 | Tauri v2 integration | ✅ | Secure IPC, capabilities, CSP, events |
| 5 | Modern UI / UX | ✅ | Dashboard, detail view, segment monitor, 60 FPS tick model |
| 6 | Queue, scheduler and system integration | ✅ | Queue limits, time windows, power actions |
| 7 | Browser integration | ✅ | MV3 extension, native host, authenticated local link |
| 8 | Batch downloading | ✅ | Lists, patterns, duplicate detection, clipboard detection |
| 9 | Media engine | ✅ | HLS / DASH, yt-dlp video pages, ffmpeg, video tools |
| 10 | Filesystem security | ✅ | path-safety: allowed roots, safe names, no link following |
| 11 | Secrets and credentials | ✅ | Sealed cookies / passwords, OS key store, redaction |
| 12 | Security engineering | ✅ | Threat model, hardening gate, policy tests, SBOM |
| 13 | Testing | ✅ | Unit, integration, crash (real processes), security, e2e |
| 14 | Observability and diagnostics | ✅ | Per-download timeline, local redacted log, reports |
| + | Extras before Phase 15 | ✅ | Smart throttle, live preview, 64 connections, neon glass UI, separate Active / Completed pages, real network / disk counters, delete from disk |
| 15 | Cross-platform system integration | ✅ | A `SystemIntegration` trait (notify, prevent sleep, shutdown / sleep, register protocol) per OS, so OS code never leaks into the downloader |
| 16 | Windows | ✅ | NSIS / MSI, protocol handler, file associations, start with Windows, **tray**, notifications; Authenticode signing; safe install folder and DLL search order |
| 17 | Linux | ✅ | AppImage / DEB (+ RPM); tray and notifications on different desktops; XDG; sleep APIs |
| 18 | macOS | ✅ | .app / .dmg, code signing, notarization, entitlements, login item, native notifications |
| + | Extras after Phase 18 | ✅ | Smart Throttle automatic mode; Smart Download Queue (several queues with rules, start times, order, express lane, timeline); Download Intelligence (pre-download analysis: resume, strategy, storage, duplicates, sign-in, temporary links, warnings) |
| **19** | Auto update | 🔜 | Signed update manifest (offline key), HTTPS, no downgrade, verify *before* install; never "download then run" |
| 20 | Performance benchmarking | ⏳ | 100 MB to 50 GB × 1 to 32+ connections; MB/s, CPU, RAM, disk, startup, UI FPS, IPC latency, DB cost |
| 21 | Release candidate | ⏳ | Gate: engine, resume, crash recovery, UI, browser, security, audit, signed installer, updater, 3 OSes tested |
| 22 | Beta | ⏳ | Internal alpha, then private beta, public beta, RC and stable; opt-in, documented crash and performance reports |

> **Phase 19 starts only when the owner says "Start phase 19".**

### Open items (known gaps)

**From the reference design**, these need data the app does not measure yet:
- the time a download took, on its completed card;
- "up to date" in the status bar, which needs Phase 19.

**Other open items:**
- release signing is wired but waits for the owner's certificates (docs/release-process.md);
- some deep Settings panels and dialogs are still English only.

---

## 9. Rules for anyone changing the code (people and AI)

1. **Keep the layers:**
   - the engine never knows about Tauri, browsers or the UI;
   - the media engine stays separate from the file engine.
2. **The UI never sends or receives file paths.** Use ids.
3. **CSP stays strict:**
   - no inline `<style>` or `<script>`;
   - styles set through the CSSOM (`el.style.x = …`) are fine;
   - only bundled assets.
4. **A new IPC command needs all of these:**
   - `command!` in `lib.rs`;
   - `build.rs`;
   - a capability file;
   - `authz.rs`;
   - the policy test.
5. **Keep in sync:**
   - `apps/desktop/src/types.ts` ↔ `crates/download-manager/src/api.rs`;
   - the EN / FR / AR keys in `i18n.ts`.
6. **Secrets never** go in plaintext, logs, URLs or error messages.
7. **All three OSes** must keep building and working: Windows, Linux and macOS.
8. **Tests:**
   - every fix or feature comes with a test;
   - e2e never triggers real power actions;
   - e2e never touches the user's real browser profile or installed app.
9. **Test ids:**
   - existing ones stay;
   - no new test id may start with `download-`, which is reserved for cards.
10. **Back up first:** before a work session, make a named backup to `C:/work/VeloxFetch-backups/`.
11. **`scripts/build-release.sh`** is run by the owner, not by an assistant.
12. **Never show invented numbers** in the UI. When a value is unknown, show "—".

---

## 10. Glossary

| Term | Meaning |
|---|---|
| **Segment** | A byte range of the file, downloaded by one connection |
| **`.part`** | The file being written; renamed when complete |
| **Tick** | The batched progress update sent to the UI ~5 times a second |
| **Gate / slot** | The queue's open / closed switch, and the number of downloads allowed at once |
| **Smart Download Queue** | Several queues with rules (time, idle, connection, busy line, big files) over the one slot scheduler |
| **Download Intelligence** | The read-only check before a download: what the link is and what to expect, each point from an observation |
| **Express lane** | One extra slot for a High / Highest download that would only wait behind less urgent ones |
| **Held** | Paused by a queue rule (or the global schedule): resumed on its own when it opens again |
| **Token bucket** | A speed limiter |
| **Smart bucket** | The bucket the Smart throttle tightens |
| **Native host** | The small program a browser is allowed to start; VeloxFetch's link between the browser and the app |
| **Capability** | Tauri's per-window list of the commands a window may call |
| **Allowed roots** | The only folders VeloxFetch may write into |
| **Video tools** | yt-dlp (video pages), Deno (needed by yt-dlp for some sites), ffmpeg (join / convert) |
