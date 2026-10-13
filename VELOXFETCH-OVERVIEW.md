# VeloxFetch: the whole app on one page

> Read this first. It explains what VeloxFetch is, what it does, how it is built, how it is kept safe, where the
> project stands (Phases 0 to 18 done, plus extras) and what comes next (Phases 19 to 22). It does not require
> reading the code. Details live in `docs/` (see the documentation map in §11).

**Contents**

1. [What it is](#1-what-it-is)
2. [What a user can do](#2-what-a-user-can-do-features)
3. [How it is built](#3-how-it-is-built-map)
4. [How the key parts work](#4-how-the-key-parts-work)
5. [Security model](#5-security-model-always-true)
6. [Where data lives](#6-where-data-lives)
7. [Build, test, release](#7-build-test-release)
8. [Roadmap](#8-roadmap)
9. [Rules for anyone changing the code](#9-rules-for-anyone-changing-the-code-people-and-ai)
10. [Glossary](#10-glossary)
11. [Documentation map](#11-documentation-map)
12. [Change log](#12-change-log)

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

**Version:** 0.1.0 (pre-release).

**Source:** git, on GitHub (`IlyesBs/VeloxFetch`):
- `main` holds what is finished;
- each feature or enhancement gets its own branch and commit (for example `feat_smart_download_queue`,
  `feat_download_intelligence`) and is merged through a pull request.

**Backups:** a named archive is made next to the project folder before each work session, for example
`VeloxFetch-backup-2026-10-08-before-download-intelligence.tar.gz`.

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
- **Download Doctor** (details in `docs/download-doctor.md`): when a download fails, the detail view says:
  - what happened;
  - where the problem is: the server, your connection, this computer, the link, the sign-in, or the content;
  - whether retrying helps: yes / later (with the server's wait) / after you fix something / no / not sure;
  - what else was seen: repeated retries, dropped connections, an expired session or link, the same failure again;
  - what to do: Retry, Restart safely, Sign in, Open download folder, or advice.

  While the app retries on its own, it says what it retries and when. Everything comes from what was observed;
  the Doctor never retries or changes anything by itself.
- **Sign in:** sites that need a login (HTTP Basic, cookies from the browser) are handled.

### Drop-down lists
Every drop-down opens a list drawn by the app in the theme's colours (same as the fields) on Windows, Linux and
macOS, never the grey system list. Keyboard, right-to-left and screen readers work. Details: `docs/ui-performance.md`.

### Video download details
Details: `docs/video-details.md`.

- Clicking a video download (site or stream) opens its own details page, like a file's.
- **Shows:** overview (percent, size or estimate, speed, time left, remaining, time taken), network (live chart,
  average, peak, retries), player, pieces, and metadata (link, site, type, quality, file type, subtitles, added /
  finished, folder, saved files).
- **Actions:** all of the list's.
- Live; returns to the list if the job is removed.

### Download again
Details: `docs/redownload.md`.

- **What:** a finished, cancelled or failed download is fetched once more from the start, on the same entry (same
  link, folder and name, queue, priority and limit).
- **Where:** the card's main button (cancelled, or finished with its file gone), the "…" menu, and the Completed
  view.
- **Missing file:** a finished file deleted outside the app is shown as no longer on disk. The list is checked
  again when the window comes back.
- **Safe:** never overwrites (a file still there keeps its name; the copy gets `name (1)`), deletes only partial
  data inside the download folders, and refuses running, paused or waiting downloads.

### Resume integrity
Details: `docs/resume-integrity.md`.

- **Every ranged answer is checked before its bytes are written:** `206`, the exact start, uncompressed, the
  download's strong ETag (when both have one), and the download's total size. Otherwise it is refused, so two
  versions of a file are never combined (`source_changed`).
- **Finished parts are hashed** (SHA-256) when they are saved, after `fsync`, in the same commit. After a crash or an
  outside change of the `.part` file, they are checked again at resume. Only a changed part is fetched again; a clean
  pause re-reads nothing.
- **Never finished from a broken segment map** (a gap, an overlap, a part not complete).
- **Join checks for servers without a version tag:** every request that continues saved bytes starts 1 KiB early,
  and those bytes must match; requests read 1 KiB past their end when needed, and every other join is compared before the file
  may finish. A same-size change with no tag (or an unchanged date) is caught, and the retry starts clean.
- A checksum given for the file still proves every byte before it is finished.

### Use a new link (expired links, sessions, mirrors)
Details: `docs/new-link-recovery.md`.

- **What:** a paused or failed download continues from a fresh link, keeping its saved bytes, but only when the new
  link is proven to give the same file.
- **Where:** the card's "…" menu (**Use a new link…**), the Doctor's advice on the Details page, and the browser
  confirmation dialog when a re-clicked link matches a stopped download (**Continue the stopped download with this
  link**, with the browser's fresh session).
- **Proof:** same size and the same strong version tag (ETag), ranges that work on the new link, and up to two
  64 KiB samples of the saved bytes that match. "Likely the same" (bytes match, no tag) waits for the user. Another
  file (other size, tag, date on the same server, bytes, or a web page) is refused. A file name or size alone never
  counts.
- **Safe:** nothing saved is ever deleted; a refused or failed check changes nothing; the same expired link is not
  retried; one check per download at a time; a cancel during the check wins; the final checksum still decides.
  Cookies and sign-in go only to the same origin.

### Download Profiles
Details: `docs/download-profiles.md`.

- **What a profile is:** a preset over existing settings: speed limit, downloads at once, connections per download,
  Smart throttle's automatic mode. It holds only what it changes, and your own settings are never overwritten, so
  Default is your settings.
- **Built-ins:**
  - Fast: up to 64 connections (the most a download uses), still adaptive; no giving way.
  - Balanced: Smart throttle automatic.
  - Low resource: 1 at once, 4 connections.
  - Plus up to 20 profiles of your own (create, duplicate, rename, edit, delete; built-ins protected).
- **One rule:** your settings + the profile's values = what is in force. Per-download / per-type limits, Smart
  throttle's cap and the queue gate are the existing layers on top; the lowest limit wins.
- **Live:** switching never restarts, resumes, pauses or cancels a download. Fewer connections give way at once; a
  lower number at once applies as downloads finish.
- **Safe storage:** one saved value. Bad data falls back to Default without touching other settings.

### Before a download: Smart Duplicate Detection
Details: `docs/smart-duplicate-detection.md`.

- **When:** pressing Add (bar or dialog) asks Rust first (`check_duplicate`: no network, the list, the history and
  a quick look at a finished file).
- **Evidence, strongest first:** the same checksum; the same link (conservatively normalised: host case, default
  port and `#fragment` only; queries and signatures kept) with its version compared when the server was asked
  (ETag / size / date); the same file under another link (strong ETag and size, same site). Never the name alone.
- **Confidence:** `exact`, `likely` or `possible` (the server's file changed since).
- **States:**
  - running: View / Download another copy;
  - paused: Resume;
  - failed: Try again (never taken for a file);
  - finished: Open (only if safe) / Show in folder, or "no longer in its folder" / "changed since";
  - history-only: through its history line.
- **Name conflicts:** a taken file name is shown in the analysis as its own row ("another file named X, contents may
  differ, saved as X (1)"), never as a duplicate. The engine never overwrites (`claim_name`).
- **Advisory:** nothing existing is cancelled, replaced or deleted. Downloading another copy always works, and two
  identical requests at once get two files.

### Before a download: Download Intelligence
Details: `docs/download-intelligence.md`.

**Where it shows:**
- the confirmation window for links from the browser or the clipboard;
- the **Check link** button in the Add dialog.

**What it shows:**

| Row | Meaning |
|---|---|
| File, type, size | What the server says (an unknown size is shown as unknown, never guessed) |
| Resume | Supported / Likely supported / Not supported / Unknown, from what the server actually did |
| Download | How it will start: parallel with N connections, one connection, careful, or the video dialog |
| Storage | Whether it fits on the drive (also the 4 GB limit of FAT32 drives) |
| Security | HTTPS or HTTP |
| Already have | The same link, the same final address, or the same file under another link (never by file name alone) |
| Warnings | Sign-in needed, temporary or expired link, a web page instead of the file, server errors, not enough space |

- **"Why?"** lists what was observed, so every label can be explained.
- A health label sums it up: Looks good, Read the warnings, Likely to fail, or Could not be checked. It is a label,
  not a score.

**How it behaves:**
- **Safe:** one quick look at the server (the same request the download makes first). Nothing is downloaded or
  written, and secrets in the link are hidden.
- **Cancellable:** answering the dialog, editing the link or closing the dialog stops the check.
- **Never blocking:** if the check fails, the download is still possible.

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
  - **Listed programs:** while a game or call app runs (LoL, Valorant, CS2, Zoom, Teams…), the gaming policy applies:
    *give way by the mode* (default: a share of the speed), *a low speed limit*, or *pause* (running downloads pause,
    none starts; only those resume when it closes, the user's own pauses stay). Detected at once, lifted after two
    scans without it (no flip-flop), kept at most 15 s if scans fail; matched by process or executable name; nothing
    the user set is changed.
- **Measured right:** the other traffic is the computer's received bytes minus VeloxFetch's own, both counted over the
  same seconds (with an allowance for packet headers, and a larger one while a limit paces downloads, when the
  system receives data by bursts). It is never fooled by VeloxFetch's own speed changing.
- **Always visible:** the cap is a share of the speed downloads really had just before (at most the best speed
  seen), so it always slows them; a share of an old best speed could be above today's speed and change nothing.
- **Steady, never stuck:** smoothed readings, separate on / off thresholds (96 / 48 KB/s), two readings to confirm
  before slowing, a calm period before speeding up (longer on an unsteady network), then steps of 1.5x. A new mode
  or minimum moves the cap at once without lifting it first. Downloads never go below the reserved minimum, so they
  never stop. On a real line: a competing download is seen within ~3 s, downloads drop to half (Balanced), and full
  speed is back ~25 s after it stops.
- **Settings:** on / off, Automatic, the mode, "Downloads always keep at least" (default 256 KB/s), and the maximum
  download speed (the global speed limit).
- **Modes:**

  | Mode | While browsing | During a game or a call |
  |---|---|---|
  | Aggressive | ~35 % | 10 % |
  | Balanced | ~50 % | 25 % |
  | Conservative | ~75 % | 50 % |

- **Shows what it does:** a status line ("Full speed — nothing else is using the connection", "Reducing speed —
  active browsing detected", "Bandwidth optimized — speeding back up"…) on the dashboard card, in Settings and in the
  status bar (one shared state: they always agree), the gauge ("25% now": the share of their usual speed downloads
  keep), and a graph of the download speed with the current limit (Settings > Speed > Smart throttle).
- **Clicks:** a switch or a mode changes on screen at once; quick clicks end where the last one says, never
  flicking back; choosing a mode while it is off turns it on in that mode.
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

### Updates
- **Settings > Updates** says whether a newer version exists, downloads it on request, and installs it: the Windows
  setup runs, an AppImage replaces itself and restarts, a `.deb` / `.rpm` / `.dmg` opens in the system's installer,
  a portable copy opens the release page. The status bar says "Up to date" or "Update x.y.z available / ready".
- **Only the owner's offline key can sign an update.** Every check: the Ed25519 signature (key compiled in), a newer
  version (never a downgrade), never older information than already seen, and the package's exact size and
  SHA-256, before it is kept and again right before it is installed. Automatic checks (on by default) only check:
  downloading and installing wait for the user. Details: `docs/auto-update.md`.

### Live preview
- The details of an audio / video download can **play it while it downloads**, from the bytes already on disk.
- **Every OS:**
  - Windows plays through the app's own protocol.
  - Linux and macOS play through a private local address (127.0.0.1 + secret, rule 12.2).
- **How it serves the player:**
  - A running download is answered like a file still being written: headers at once, bytes as they arrive, never a
    "not there yet" that stops the player.
  - The start of the file is fetched first, even under a speed limit.
  - The player reconnects by itself, and says plainly when a format cannot be played on this computer.
- **Linux packages:** the `.deb` and `.rpm` recommend the GStreamer plugins the player needs (a system that cannot
  install a codec still installs the app), and the AppImage carries an allow-list of software playback plugins,
  never the machine's hardware ones (they crashed it at start on other computers). Details: `docs/smart-throttle-and-preview.md`.

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
| `crates/download-manager` | What the UI talks to: every action, validation, queue / schedule / power, events, metrics, videos, previews, Download Intelligence |
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
| `tests/e2e` | Drives the real app window (WebView2 on Windows, WebKitGTK on Linux) |
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
  - halves on errors, or when the speed collapses for 3 seconds in a row (never below 4 connections for a
    falling speed alone: with few connections, fewer only means slower);
  - reacts **at once** to HTTP 429 / 503 (the server says "too many connections"), once (the retries that follow
    are not counted again).
- **Steady speed:** every "no" (a server limiting connections, a probe that brought nothing) doubles the wait before
  the next probe (15 s → 8 min), so the speed does not swing between full and nearly nothing. Measured on a real
  server allowing 2 connections: before, 4 → 2 → 1 → 2 → 4 every ~17 s (0.9 to 2 MB/s); after, 2 steady connections.
- **Speed limits:** when a limit holds the speed (a connection had to wait for it, or ≥80 % of it), the controller
  changes nothing. A capped speed says nothing about the network.
- **Limits changed while running:** a lower limit never stops downloads (what they owed is kept in time, not
  bytes); a higher one speeds them up at once, without a burst followed by a stop. Bytes count when written, at
  least every 200 ms, so the progress and the speed shown move smoothly even at a low limit; the shown speed is
  smoothed over ~2 s.
- **Limits are token buckets:**

  | Bucket | Set by |
  |---|---|
  | global | the user |
  | per category | the user |
  | per download | the user |
  | smart | the Smart throttle |

  The tightest one governs.
- **Measured:** on a real ~24 Mbit/s line, 64 connections, a stable 2.6 to 2.9 MiB/s (the line's maximum).
- **Slow servers (issue #21):** the download's own lowerings are soft. They stop new requests and the ones still
  waiting for an answer, never a transfer; only a speed limit or the user's settings cut transfers. Joins are
  checked from 1 KiB tails, not from extra requests at the end. A one-connection transfer that breaks continues from
  its saved bytes when the server proves it is the same file. See `docs/slow-servers.md`.

### 4.3 Live UI at 60 FPS
- The manager never forwards each network write. Every 200 ms it sends **one "tick"**: all progress plus the
  global metrics. It goes quiet when idle.
- The UI store applies events once per animation frame. Untouched cards do not re-render.
- **Network and Disk panels:**
  - they show real system counters (through `sysinfo`): the whole PC's download / upload, and the read / write
    of the download disk;
  - loopback, tunnels and capture drivers are skipped;
  - they refresh once a second, idle too.
- **Smooth on Linux too** (details in `docs/ui-performance.md`): the Linux web view paints on the CPU, so there the
  interface has no shadows (selected and focused rings become outlines), and the window-wide backdrop is a picture
  drawn once on every system. Six running downloads: 23 -> 62 fps. Linux's web view itself stops at 60 fps, whatever
  the screen; Windows uses the screen's rate.
- **No flashing:** starting, resuming or pausing a download never makes the cards fade out and in (each card fades
  in once, when it is new), and the connection grid changes its pulse smoothly (issue #20).
- **Starts fast:** the window appears as soon as the interface is on the page (~60 ms of page time, it was ~310 ms),
  over the interface's own background colour (no dark flash before a light theme).

### 4.4 Browser hand-off
- The download passes through several checks:
  1. the extension validates it;
  2. it goes through native messaging to `velox-native-host`, which validates again;
  3. it crosses the local pipe / socket (mutual HMAC authentication, a new token every run);
  4. the app validates a third time;
  5. a confirmation window opens (with a "Sign in to this site" part, opened by itself when the server asks for a
     user name and password);
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
  name alone; with a confidence and version comparison), a taken file name (`nameConflict`), findings and a health
  label.
- **Where it shows:** `PendingDto.analysis` (confirmation dialog), and the `analyze_link` command (Add dialog, main
  window only).
- **Cancelling:** answering the dialog, a new check, or `url: null` cancels the check.
- **Secrets:** the shown link hides signatures and tokens; the result carries the final host only, error codes
  only, and is never logged.

### 4.8 Download Doctor
- **What it reads (read-only, only after a failure):**
  - the engine's error (transport errors by their causes only, never the message that names the link);
  - the run's events and retry count, and the saved diagnostics (earlier bytes, the previous failure);
  - the record (session, resumable), the link (signed / expired), and the computer's network state.
- **Rules** (`doctor.rs`, pure): category, where the problem is, 5 retry classes, severity, contributing factors,
  and actions (only real buttons; the rest is advice).
- **Where it shows:** `DownloadDto.doctor` (failed, or live while retrying), `DiagnosticsDto.doctor`, and one
  timeline line.
- **Recovery** stays the engine's: its retry loop with backoff and `Retry-After`, and safe resume / restart. One fix:
  certificate failures are no longer retried.
- **Get a new link:** its `newLink` advice has a real button (Use a new link…). The new link is verified before any
  saved byte is reused (`docs/new-link-recovery.md`).

---

## 5. Security model (always true)

**Mindset:** assume every external input is hostile. That covers URLs, HTTP answers, file names, headers, browser
messages, downloaded files, update data and UI requests.

- **WebView:**
  - strict CSP: no inline scripts or styles in HTML, no remote origins, no `eval`;
  - the page cannot make network requests.
- **IPC:**
  - 82 fixed commands, each its own permission (most are main-window only);
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
- **Updates:** only a manifest signed with the owner's offline Ed25519 key is believed (public key compiled in,
  HTTPS, no downgrade, no replay); the package's size and SHA-256 are checked before it is kept and before it is
  installed; nothing installs without the user (`docs/auto-update.md`).
- **Release gate:** `scripts/security-gate.mjs` runs cargo-audit / deny, npm audit, a policy test, an SBOM and
  checksums.

More: `docs/security.md`, `docs/threat-model.md`, `docs/PRIVACY.md`.

---

## 6. Where data lives

| What | Where |
|---|---|
| Downloads, segments, settings, history, diagnostics | SQLite file in the app data folder |
| Download Intelligence | nothing: each check lives in memory only and is dropped when the dialog is answered or closed |
| Download Doctor | the last diagnosis of each download, inside its saved diagnostics (SQLite `diagnostics` table, JSON); a copy in memory for the list |
| Smart Download Queue | same file: each download's queue, place and start time in the `queue` table; the queues and their rules in the settings (`queues.list`, `queues.state`, `queues.held`) |
| Sealing key for secrets | OS credential store (Windows Credential Manager / macOS Keychain / Secret Service) |
| Log | `<app data>/logs/velox.log` (rotating, redacted) |
| Video tools | `<app data>/tools` |
| Updates | `<app data>/updates` (only the current package); settings `update.auto`, `update.checked`, `update.released` |
| UI-only preferences (folded sidebar, density, sort, open queue tab, language, theme) | `localStorage` |
| How the browser helper starts the app | `<app data>/app-path` (written by the app at each start) |
| Start at login (when turned on) | Windows `HKCU\...\CurrentVersion\Run`, `~/.config/autostart/app.velox.fetch.desktop`, `~/Library/LaunchAgents/app.velox.fetch.plist` |
| Second-launch signal (Linux / macOS) | `<app data>/show.sock` (or the user's runtime folder when the path is too long) |

Nothing is sent to any server except the downloads themselves and the anonymous update check (two small public
files from GitHub; it can be turned off: `docs/PRIVACY.md`).

---

## 7. Build, test, release

```bash
cargo test --workspace                    # engine, manager, integration and crash tests
npm --prefix apps/desktop test            # UI unit tests (vitest)
cd apps/desktop && npm run tauri dev      # run the app (dev)

# end-to-end on the real app (Windows: WebView2; Linux: WebKitWebDriver + Xvfb):
npm --prefix apps/desktop run build
cargo build -p velox-desktop --features velox-desktop/custom-protocol -p velox-native-host -p velox-integration-tests
node tests/e2e/desktop-<suite>.mjs
#   suites: ipc, ui, redesign, views, usability, queue, smart-queue, intelligence, folders, batch, media, video,
#           preview, diagnostics, browser, throttle, gaming, perf, updates   (ipc also needs `cargo build --bin velox`)

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

**Linux AppImage:** the script always checks for, and installs, what the AppImage's media bundle needs:
* `patchelf`, from the package manager, or else from pip into `~/.local/bin`;
* the GStreamer plugins, with their folders found on Debian / Ubuntu, Fedora / openSUSE and Arch.

If they still cannot be installed, it builds the AppImage without the bundle and warns; it never fails for this.
Only the allow-listed software playback plugins go in (`scripts/appimage-media.sh`: 44 instead of 273 on Debian 13);
VA-API, NVIDIA, QSV, V4L2, Vulkan and KMS plugins never do. The `.deb` / `.rpm` only recommend the media packages.
Details: `docs/release-process.md`.

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
| + | Extras after Phase 18 | ✅ | Smart Throttle automatic mode; Smart Download Queue (several queues with rules, start times, order, express lane, timeline); Download Intelligence (pre-download analysis: resume, strategy, storage, duplicates, sign-in, temporary links, warnings); Download Doctor (failure diagnosis: cause, where, retry class, severity, factors, actions); Smart Duplicate Detection (a link the app already has, in any state, with a confidence and the finished file's state; a taken file name told apart from a duplicate; advisory, never replaces anything); Download Profiles (Default / Fast / Balanced / Low resource and your own: presets over existing settings, one precedence rule, applied live without restarts); Download again (a finished, cancelled or failed download fetched again from the start on its own entry; a deleted file shown as missing; never overwrites); Video download details (a details page for every video download, like a file's) |
| + | Fixes after Phase 18 | ✅ | Smart throttle that really applies (same-window traffic measurement, a cap from today's speed, no fake browsing while capped), no stop when switching it or its modes, its card / Settings / status bar always agreeing; a steady download speed (no 4 → 2 → 1 connection cycling, no burst-then-stop when a limit changes, smooth progress at low speeds); a smooth interface on Linux (no costly shadows, the backdrop as a picture: 23 -> 62 fps with downloads running) and a window that appears as soon as the interface is ready, in the theme's colour; a gaming policy for Smart throttle (give way / low limit / pause, videos too, programs by name or full path, debounced detection, safe on scan failure and restart); live preview on Linux and macOS (private local address, growing-file answers, start first under a speed limit, media plugins installed with the Linux packages); every known test failure fixed; a steady speed on servers that limit connections (a refusal holds new requests back instead of a storm of refusals, runs of parts per request) and a Smart throttle cap that holds on the line (only the connections it needs, short requests, no stop at the start, no false browsing, no window flashing); up to 64 connections everywhere (the Fast profile and files of 10–200 MB were held at 32 or 16); drop-down lists drawn by the app in the theme's colours on every system (the system list was grey on Linux); the release script's AppImage step (patchelf and media plugins always installed, never a failed release for them) |
| 19 | Auto update | ✅ | Ed25519-signed manifest (offline, passphrase-encrypted key; public key compiled in), HTTPS, no downgrade, no replay, size + SHA-256 verified before keeping and before install; Windows setup / AppImage self-replace / system installer / release page; Settings > Updates and the status bar |
| 20 | Performance benchmarking | ⏳ | 100 MB to 50 GB × 1 to 32+ connections; MB/s, CPU, RAM, disk, startup, UI FPS, IPC latency, DB cost |
| 21 | Release candidate | ⏳ | Gate: engine, resume, crash recovery, UI, browser, security, audit, signed installer, updater, 3 OSes tested |
| 22 | Beta | ⏳ | Internal alpha, then private beta, public beta, RC and stable; opt-in, documented crash and performance reports |

> Phase 19 was started by the owner on 2026-10-13 ("start the phase 19 to 21").

### Open items (known gaps)

**From the reference design**, these need data the app does not measure yet:
- the time a download took, on its completed card;

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
   - `apps/desktop/src/types.ts` ↔ `crates/download-manager/src/api.rs` (and `intelligence.rs`);
   - UI text lives in `apps/desktop/src/locales/{en,fr,ar}.json`, with the same keys in each.
   - The UI and the installer never name tools or technologies (yt-dlp, ffmpeg…): they say "required dependencies".
6. **Secrets never** go in plaintext, logs, URLs or error messages.
7. **All three OSes** must keep building and working: Windows, Linux and macOS.
8. **Tests:**
   - every fix or feature comes with a test;
   - e2e never triggers real power actions;
   - e2e never touches the user's real browser profile or installed app.
9. **Test ids:**
   - existing ones stay;
   - no new test id may start with `download-`, which is reserved for cards.
10. **Back up first:** before a work session, make a named backup archive next to the project folder.
11. **`scripts/build-release.sh`** is run by the owner, not by an assistant.
12. **Never show invented numbers** in the UI. When a value is unknown, show "—".
13. **One feature, one branch:** check first whether the feature exists. If it does, add only the enhancement; if
    nothing new is asked, skip it. Commit on a branch named after the feature.
14. **Document every task:** this overview, the README, `docs/architecture.md`, `docs/security.md`,
    `docs/threat-model.md`, `docs/manual-test.md`, `docs/testing.md`, and the feature's own document.
15. **Updates:** never add another way to fetch or run a new version. Only `crates/update-verify` decides whether an
    update is genuine, and only the owner signs, offline (`docs/auto-update.md`).

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
| **Download Doctor** | The diagnosis of a failed (or retrying) download: what happened, where the problem is, whether retrying helps, what to do |
| **Retry class** | Download Doctor's answer to "will retrying help?": retryable, retryable after a wait, after a user action, not retryable, unknown |
| **Probe** | The quick first look at a link (`HEAD`, then a 1-byte range request) that tells size, type and resume support |
| **Pending download** | A link from the browser or clipboard waiting in the confirmation window; nothing is stored until it is accepted |
| **Health label** | Download Intelligence's summary (good / caution / problem / unknown), taken from its findings, never a score |
| **Finding** | One observation of Download Intelligence (for example "plain HTTP" or "not enough space"), with a level: info, warning or problem |

---

## 11. Documentation map

| Topic | Document |
|---|---|
| How the code is organised | `docs/architecture.md` |
| Security rules and what each feature does to stay safe | `docs/security.md` |
| Threats and how each one is handled | `docs/threat-model.md` |
| Privacy (what is read, nothing is sent) | `docs/PRIVACY.md` |
| Automatic tests, how to run them, known results | `docs/testing.md` |
| Checks done by hand on each OS | `docs/manual-test.md` |
| Building and publishing a release | `docs/release-process.md`, `docs/store-publishing.md` |
| Smart throttle and live preview | `docs/smart-throttle-and-preview.md` |
| Interface smoothness, start-up, themed drop-down lists | `docs/ui-performance.md` |
| Smart Download Queue | `docs/smart-download-queue.md` |
| Download Intelligence | `docs/download-intelligence.md` |
| Smart Duplicate Detection | `docs/smart-duplicate-detection.md` |
| Download Profiles | `docs/download-profiles.md` |
| Download again | `docs/redownload.md` |
| Use a new link (expired links, sessions, mirrors) | `docs/new-link-recovery.md` |
| Resume integrity (segment hashes, source changes) | `docs/resume-integrity.md` |
| Slow servers (as fast as a browser) | `docs/slow-servers.md` |
| Updates (signed, offline key, install per system) | `docs/auto-update.md` |
| Video download details | `docs/video-details.md` |
| Download Doctor | `docs/download-doctor.md` |
| Design decisions over time | `docs/adr/` |
| Look and feel (reference images, screenshots) | `docs/design/`, `docs/screenshots/` |

---

## 12. Change log

Newest first. Each line is one branch and one commit, with its doc. Every entry came with tests, and the full suite
was green (Rust, vitest, policy, e2e on Linux).

### October 2026 (second half)

| Date | Branch (commit) | Kind | What changed | Doc |
|---|---|---|---|---|
| 2026-10-13 | `phase19_auto_update` | Phase 19 | Updates: Settings > Updates and the status bar. A manifest signed with the owner's offline Ed25519 key (`scripts/update-key.mjs`: passphrase-encrypted, terminal only, never in CI; public key in `update-keys.txt`, compiled in) is verified strictly before it is read (`crates/update-verify`); no downgrade, no replay; the package is kept only after its exact size and SHA-256 match, checked again before install; Windows setup run under a read-only-sharing lock, AppImage replaced atomically and restarted, `.deb` / `.rpm` / `.dmg` opened in the system's installer, portable copies get the release page. Automatic checks (on, can be turned off) never download or install by themselves. Six main-window commands, none taking an address or a path. | `docs/auto-update.md` |
| 2026-10-12 | `fix_slow_server_speed` | Fix | Issue #21: a slow server downloaded slower than in a browser. (1) Lowering the connection count (a step that brought nothing, a dip, errors, 429 / 503) cut the connection that was transferring and kept the requests still waiting for an answer: now requests waiting give way and transfers finish (`set_limit_soft`); only a speed limit or the user's settings cut transfers. (2) Joins of parts started side by side were checked at the end, one request after the other: now from 1 KiB tails read with the request, the rest 4 at a time. (3) A one-connection transfer (server slow to answer) restarted from byte 0 after each broken connection: now it continues with `Range` + `If-Range` when the server proves the same file (206 at the offset, same total and version, last KiB identical), else from 0 as before. On a 10–50 KB/s server answering in 2 s: 226 s → 213.6 s next to curl's 210 s (the first check's wait is the difference). | `docs/slow-servers.md` |
| 2026-10-12 | `fix_diagnostics_e2e_retry` | Test | `desktop-diagnostics.mjs` pressed the error box's last button, which became "Use a new link…" when that feature came: the details' Retry button now has its own id (`detail-retry`) and the test uses it. No change in behaviour. | `docs/testing.md` |
| 2026-10-12 | `fix_pending_signin` | Fix | The browser confirmation popup can take the site's user name and password: its "Sign in to this site" part opens by itself when the check found that the server asks for them. Checked before the request is taken (a mistake keeps it waiting), sealed with the download like the Add dialog's, never shown back or logged. Without it, the card and details still offer Sign in. | `docs/security.md` |
| 2026-10-12 | `fix_card_flash` | Fix | Issue #20: starting a download made the cards flash. The entrance animation (fade in from nothing) replayed on every card React moved (a download going up the list) or re-created (a card changing section on Active): it now plays once per card (`ui/entrance.ts`), for Smart queue rows too. The connection grid's pulse changed its duration with the speed, so all lit cells jumped at once: the pace now changes through the playback rate. | `docs/ui-performance.md` |
| 2026-10-12 | `fix_linux_packaging_media` | Fix | Linux packages: the `.deb` required `gstreamer1.0-libav` (and base / good), so `apt` refused to install on systems whose FFmpeg libraries are pinned or mixed ("held broken packages"): the media packages are now Recommends in the `.deb` and `.rpm`, and the preview says "can't play" when a codec is absent. The AppImage crashed at start (`Segmentation fault`) because the bundler copied all 273 system GStreamer plugins, hardware ones (VA-API, NVIDIA, QSV, V4L2, Vulkan, KMS) included: the build now stages an allow-list of 44 software playback plugins, with a deny rule for hardware / device / network plugins, and the AppImage keeps its own GStreamer cache. | `docs/release-process.md` |
| 2026-10-12 | `fix_npm_advisory` | Fix | The UI bundler's `source-map-js` moved from 1.2.1 to 1.2.2 (GHSA-68fv-2mgg-jv7q, a denial of service through crafted source maps; build time only, never shipped). Lockfile only. The release gate passes in full. | `docs/security.md` |
| 2026-10-11 | `feat_chunk_reverification` | Feature / Fix | Resume integrity: per-segment SHA-256 saved with the segment (after `fsync`, same commit) and re-verified at resume when the `.part` file changed since (crash, outside change), so only changed parts are fetched again. Ranged answers refused when compressed, carrying another strong ETag (a server ignoring `If-Range` could mix two versions: fixed), or stating another total. Join checks (1 KiB overlaps and tails at every join) catch a same-size change even with no version tag; after a source change the saved part is never continued. The segment map is checked at resume and before finishing. New failure `source_changed` in Download Doctor. Schema v7 (backward compatible). | `docs/resume-integrity.md` |
| 2026-10-11 | `feat_session_link_refresh` | Feature | Use a new link: a paused or failed download (expired signed link, ended session, mirror gone) continues from a fresh link (pasted, from the Doctor, or re-clicked in the browser). Saved bytes are reused only on proof of the same file: strong ETag + size + matching 64 KiB samples, and working ranges and a validator. "Likely" needs the user's word. Another file is refused and nothing changes. Same-origin credentials only, no secrets in logs, bounded (no retries, one check at a time). New command `use_new_link` (main window only); `Downloader::sample_range`. | `docs/new-link-recovery.md` |
| 2026-10-11 | `fix_select_colors` (55e4736) | Fix | Every drop-down opens a list drawn by the app in the theme's colours (same surface as the fields), on Windows, Linux and macOS. The system list ignored all CSS: grey on Linux. Keyboard, right-to-left, ARIA. The field has the same surface as other fields. No component changed. | `docs/ui-performance.md` |
| 2026-10-11 | `fix_connections_64` (07c8002) | Fix | Up to 64 connections everywhere: the Fast profile is 64 (was 32). The planner no longer holds files of 50–200 MB at 32 or 10–50 MB at 16: from 10 MB a download may use all 64, still bounded by the setting and the 256 KB minimum piece. | `docs/download-profiles.md`, `docs/testing.md` |
| 2026-10-10 | `feat_video_details` (2d55185) | Feature | Clicking a video download opens a details page like a file's. It shows the overview, live speed chart, average and peak, retries, time taken, size or estimate, the player, the pieces strip, and the metadata (link, site, type, quality, file type, subtitles, added / finished, folder, saved files), with every action. The job sends a running-time clock that does not count paused time. The row's "left" / "pieces" texts are translated. | `docs/video-details.md` |
| 2026-10-10 | `feat_redownload` (ba34df2) | Feature | Download again: a finished, cancelled or failed download is fetched again from the start, on its own entry (same link, folder and name). It never overwrites: if the file is there, the copy gets `name (1)`. A finished file deleted outside the app is shown as missing, and the list resyncs when the window comes back. New command `redownload` (main window only). | `docs/redownload.md` |
| 2026-10-10 | `fix_release_appimage_media` (7c65308) | Fix | `build-release.sh`: the AppImage failed (`failed to run linuxdeploy`, "patchelf not found") on machines that already had the build libraries. The script now always checks and installs `patchelf` (pip fallback) and the GStreamer plugins, tells the bundler where they are on every distribution, and falls back to an AppImage without the media bundle instead of failing. | `docs/release-process.md` |
| 2026-10-10 | `feat_download_profiles` (8fcc904) | Feature | Download Profiles: Default, Fast, Balanced, Low resource and your own. They are presets over the speed limit, downloads at once, connections and Smart throttle's automatic mode, with one precedence rule. They apply live, with no restarts; progress is kept and a pause respected. Status-bar switch. | `docs/download-profiles.md` |
| 2026-10-10 | `fix_preview_linux_macos` (04c641e) | Fix | Live preview on Linux and macOS: a running download is answered as a growing file (the player treated "not yet" as fatal), its start is fetched first, macOS uses the private local address, and the Linux packages install the media plugins (the AppImage bundles them). | `docs/smart-throttle-and-preview.md` |
| 2026-10-10 | `feat_smart_duplicate_detection` (a65e7bd) | Feature | Adding a link the app already has says so before anything happens, by state and confidence, with the finished file's state. A taken file name is kept apart from a duplicate. It is advisory and never replaces anything. | `docs/smart-duplicate-detection.md` |
| 2026-10-10 | (408fdc8) | Fix | A steady download speed on servers that limit connections. Smart throttle's cap holds on the line, never stops downloads, and the window no longer flashes. | `docs/smart-throttle-and-preview.md` |
| 2026-10-09 | (8077e0f, 5edc0f3) | Fix / Feature | Smart throttle gaming policy (give way, low limit or pause; videos too; programs by name or path). Every known test failure fixed. | `docs/smart-throttle-and-preview.md` |
| 2026-10-08 – 09 | (e593bc7, 27b0d90, d5a95f7) | Fix / Perf | Smooth interface on Linux (23 → 62 fps with downloads running), the window shows sooner, and Smart throttle really applies. | `docs/ui-performance.md` |

Earlier work (Phases 0–18 and the extras before them) is in §8 and `docs/`.
