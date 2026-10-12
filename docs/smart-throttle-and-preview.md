# Smart throttle and Live preview

Two isolated features added on top of the download engine. Neither changes how connections are made,
how segments are planned or how files are written; each one enters the engine through a small hook.

## Touchpoints (the only places the existing code was changed)

| # | Where | What | Why |
|---|---|---|---|
| 1 | `crates/downloader-core/src/limiter.rs`, `RateLimits` (`smart` bucket, `set_smart`, and `throttle_for` adds it) | one more token bucket that every download passes through, unlimited unless Smart throttle sets it | the cap applies at once to running downloads, separately from the user's own limits, and lifting it restores them exactly |
| 2 | `limiter.rs`, `TokenBucket::changes` + `Throttle::take` | a wait re-checks every 100 ms whether a limit changed and stops early if so | lifting a cap (or raising any limit) is instant even for connections that were already waiting under the old one |
| 3 | `crates/downloader-core/src/disk.rs`, `pump` | the end-of-range check moved below the limiter call | **bug fix**: the last chunk of every range was never charged, so with a fast server (a whole range in one chunk) no limit applied at all, the user's own limits included |
| 4 | `crates/downloader-core/src/engine.rs`, `DownloadControl::{set_preview, preview}` (`Arc<AtomicBool>`) | a per-download flag | asks for preview order |
| 5 | `engine.rs`, `run_parallel` (`preview` argument, one `for` line) + `planner::preview_order` | in preview order the segments start as: first, **last**, then the rest in order | players read the start and the index at the end (many MP4s) before they show anything; the set of segments, the connection count and the retry logic are unchanged |
| 6 | `crates/download-manager/src/manager.rs` | `mod preview; mod smart_throttle;`, two state fields, `control.set_preview(self.preview_wanted(id))` when a run starts | wiring only |
| 7 | `apps/desktop/src-tauri/src/lib.rs` | `.register_asynchronous_uri_scheme_protocol(preview_server::SCHEME, preview_server::handle)`, `smart_throttle::spawn(manager.clone())`, 4 commands | Tauri v2 wiring |
| 8 | `tauri.conf.json` | `media-src velox-preview: http://velox-preview.localhost` | the `<video>` may read the preview protocol; scripts still cannot (`connect-src` unchanged) |

## Smart throttle

```text
src-tauri/src/smart_throttle.rs   every 3 s (Skip on missed ticks), only while enabled:
    sysinfo: process NAMES + executable FILE NAMES only (ProcessRefreshKind::nothing().with_exe(OnlyIfNotSet),
             ~7 ms for 530 processes in release, the same as names alone), on a blocking thread, 2 s limit
    + automatic mode: SystemIntegration::user_idle()   (time since the last input only; ~1-2 ms)
    -> manager.smart_throttle_observe_activity(names, idle)      (on the same blocking thread)
                                                                  timeout / panic -> observe(None) = unknown
download-manager/src/manager/smart_throttle.rs
    measure: the computer's received bytes (SysIo::net_received_total, read now) and VeloxFetch's received bytes
             (session total + what running downloads received since their last tick), at the same moment;
             other = (computer delta - VeloxFetch delta x 1.06 headers) / seconds since the previous observation
             (signed; first reading, a gap over 30 s or counters that went back start over; two calls closer
             than 1.5 s reuse the last window)
    reference = what downloads got lately while nothing held them back (at most the best speed seen)
    listed program running?   GameWatch (on at the first sighting, off after 2 scans without, kept <= 15 s
                              through failed scans)
                              yes -> by gaming policy: adaptive = mode share of the reference (at least the
                                     reserved minimum); low = the chosen limit; pause = the reserved minimum and
                                     the queue held (sync_game_hold)
    else automatic mode?      Controller::step_with(other, idle, mine, paced, peak, reference, share, minimum)
                              -> cap or full speed, and why
    -> RateLimits::set_smart(cap)    (only when it changed; logged with the reason)
```

### Gaming policy (October 2026)

What downloads do while a listed program (a game, a call) runs. Settings > Speed > Smart throttle > "While a listed
program runs"; `SetSmartThrottle { game_policy, game_limit_bps }`; saved as `smart_throttle.game_policy` and
`smart_throttle.game_limit_bps`.

| Policy | Smart bucket while the program runs | Queue | Default |
|---|---|---|---|
| `adaptive` | the mode's share of what downloads got lately (at least the reserved minimum), as before this change | untouched | yes (no change for existing users) |
| `low` | the chosen limit (32 KB/s - 1 GB/s; default 512 KB/s), never zero | untouched | |
| `pause` | the reserved minimum (for what the queue cannot pause: video jobs) | gate closed, running downloads paused and remembered (`held`), nothing new starts | |

**Detection** (`GameWatch`, pure, unit tested with a given clock): the listed name (or the executable's file name)
seen once applies the policy; it is lifted after `GONE_AFTER_SCANS` = 2 successful scans in a row without any listed
program (3-6 s: a launcher handing over to the game, a program restarting or a flicker never lift it in between).
Another listed program taking over keeps it on without a gap. A failed scan (`observe(None)`: listing failed, took
over 2 s or panicked) keeps a policy in force for `SCAN_FAIL_GRACE` = 15 s, then lifts it; with no program known, a
failed scan finds none. Matching is on whole names, without case or `.exe` (a 15-byte Linux name may match a longer
listed name it starts); launchers, helpers and look-alikes (`steam`, `cs2-helper`, `GTA5Launcher`, `teamviewer` for
`Teams`) never match. The executable's file name counts because a program may rename its main thread (Linux then
shows e.g. `MainThread`) and is not cut at 15 bytes; its folder is never kept. A listed entry may also be a full
path (`C:\Games\Riot\Game.exe`, `/opt/game/run`; absolute, no wildcards, up to 260 characters, `is_app_path`):
it matches that executable only, so two programs with the same file name are told apart. Executable paths are read
from the system only while such an entry is listed, compared and dropped (`/` and `\` and case do not matter).

**Precedence** (highest first):

1. Smart throttle off: no gaming policy at all (turning it off during a game lifts everything at once).
2. A listed program runs: the gaming policy replaces the automatic mode (its controller starts over afterwards).
3. Otherwise the automatic mode (if on), else full speed.
4. Every limit applies at once, the tightest wins: the user's global, per-kind and per-download limits and the
   smart bucket are separate buckets, so the gaming policy never changes the user's limits, and lifting it restores
   exactly what the user has, including changes made during the game.
5. Queue gate = not paused by the user **and** inside the schedule window **and** not held for a game. The game
   closing opens it only if the other two agree; what the game paused resumes when the gate opens.

**Transitions** (`queue.rs`, `sync_game_hold`): one at a time (`game_hold_lock`) and always towards the newest wish,
so a game that closes while its downloads are still pausing is let go right after; idempotent (a repeated scan
changes nothing). Holding uses the queue's existing `hold_running` (pause, wait, remember in `held`, saved) also
when the user had already closed the gate without pausing running downloads; letting go uses the existing
`release_held` (only what is still paused, at the front of the queue). Video jobs (streams, video pages) are paused
the same way (`hold_media_for_game`, remembered in memory) and resumed when the game closes
(`release_media_for_game`, only those still paused; one the user resumed during the game is theirs). The hold itself is in memory only: after a
restart nothing is held, the remembered downloads resume at launch, and a game still running is seen again within
3 s. A download the user pauses during the game is taken off `held` (`forget_held`) and stays paused; one the user
starts during the game waits for the gate; one added during the game starts after it.

**Shown**: the card, the status bar and Settings say "{app} is running · limited to {speed}" (adaptive, low) or
"{app} is running · downloads paused until it closes" (pause), only while a program is really detected; the Queue view
says "Paused while a game or call runs · N will resume when it closes". No notification.

**Limits**: launchers whose game runs under another name need that name in the list; a video job the user starts
during the game (pause policy) runs at the reserved minimum; another user's or a protected process shows its name
only (its executable is not readable), which is enough for name matching, not for path matching.

### What was wrong, and the fixes (October 2026)

Measured on a real line (~3 MB/s, real servers, a competing download started by hand):

| Symptom | Cause | Fix |
|---|---|---|
| Slowed for nothing at start, then never lifted | "other traffic" = the dashboard's network reading of the *previous* second (refreshed in the background, read before it was done) minus VeloxFetch's speed of *now*: a speed change looked like 1.5 MB/s of someone else; then 24 KB/s was enough to stay slowed | both counters read at the same moment over the same window (`SmartState::measure`), headers allowed for, thresholds 96 KB/s on / 48 KB/s off |
| "Never works": no visible effect | the cap was a share of the best speed ever seen (maybe another, faster server): often above what downloads got, so it changed nothing | the cap is a share of the **reference**: what downloads got lately while free (`recent_bps`), at most the best speed |
| Stays slowed after the other traffic stopped | while capped, the system takes data in by bursts ahead of VeloxFetch reading it: each 3 s window is off by up to ±100 % of the speed, and clipping each window at 0 kept only the "+" | signed windows, the last two averaged, then smoothed (bounded below), and a larger noise allowance while any limit paces downloads (`paced`: 30 % of VeloxFetch's speed, 3 % otherwise) |
| Stops, then returns, when switching modes | a new mode or minimum restarted the controller: full speed for a few readings, then slow again; with video jobs each step restarted yt-dlp | `Controller::retarget`: the cap moves at once and keeps its state; yt-dlp restarts only for a limit appearing / going or at least 2x off, seen on 2 checks in a row |
| Bursts and pauses under a cap | engine side: see "Limits that change while downloads wait" and "Bytes count when written" in `architecture.md` | debt kept in time on a lower limit, forgiven on a higher one; buffers written before waiting |
| Downloads on 1 connection | a 429 was counted twice (halved, then the retries halved again) | see "Steadiness" in `architecture.md` |

Result on the real line: no false start, the competing download is seen within ~3 s and downloads drop to 50 %
of their real speed (Balanced), the cap holds while it lasts, then eases back up and full speed returns about
25 s after it stopped; 2 steady connections throughout; switching on / off and modes every 3 s never stops the
download.

### Steady speed, a cap that holds on the line, no flashing (October 2026)

Reported: the speed swung 3 MB/s → 1 MB/s → 200 KB/s on a 3 MB/s line; turning Smart throttle on made the window
flash, downloads stopped a while and came back, and the throttle "did not work". Measured on that line with real
servers (each 200 ms tick, a temporary probe through the manager, as the app runs it):

| Symptom | Cause | Fix |
|---|---|---|
| Speed falling to almost nothing for seconds on servers that limit connections | a refused request (429) freed its slot for the next part, refused too: 40 refusals in a row, 0 bytes; one refusal at 2 connections halved to 1 for minutes | the download's new requests wait after a refusal (1 s, 2 s… while it keeps refusing; a burst counts once); a single refusal after a steady 20 s keeps the count; a request carries a run of parts (16× fewer requests). See `architecture.md` > Performance model |
| Under a cap: 30–90 % above it for 10+ s, then everything stopped ("stops a while, then resumes") | every connection's system buffer, grown at full speed, kept filling above the cap; reads of up to 408 KB paid after reading built a 3.5 s backlog everyone waited for | a capped download keeps only the connections the cap needs (the others stop at once, handing back what they had not paid), short requests on fresh connections, chunks paid in 64 KB pieces, what was owed to an old cap settled on any change |
| Slowed for "browsing" with nobody else on the line, and kept slow | (a) the noise allowance (3 % of the speed) was below what a 3 s window is off by at full speed; (b) right after slowing, the system's intake above the cap looked like someone else | 10 % allowance and 3 confirmations; 2 readings after a cap change are ignored; with few connections the intake is small and short |
| The next cap a share of the capped speed | the reference was learned the moment the cap lifted | learned again 3 observations later |
| The window flashed every 3 s while Smart throttle was on | each poll wrote the same value to `<html data-throttle>`; the theme watcher took it as a colour change and painted the full-window background picture again, twice | written only when it changes (`throttle.ts`), and the watcher repaints only when the palette changes (`theme.ts` `paletteKey`: "on" and "limiting" share colours) |

Result on the real line (thinkbroadband, 3 MB/s): free 2.7–3.3 MB/s; a game at 25 % → ~730 KB/s within a second,
steady (no second without bytes), 6 connections; the game closes → back to full speed in ~5 s; automatic mode on
for 50 s with nobody else: never slowed. Tests: see `docs/testing.md` > "Steady speed and a smooth Smart throttle".

### Automatic mode (`Controller`)

| Signal | Rule |
|---|---|
| Smoothing | the last two (signed) readings are averaged, then each reading moves the smoothed traffic half way (`SMOOTHING` 0.5); it may go below zero by at most max(96 KB/s, 10 % of VeloxFetch's speed) |
| Noise | of VeloxFetch's own speed, 10 % (30 % while a limit paces downloads: `paced`) never counts as someone else (3 % until October 2026: a window 5–8 % off at full speed, common on a real line, slowed downloads for nobody right after turning it on) |
| Using the internet | the user is not away (idle < 2 min, or unknown) and other traffic > max(96 KB/s, noise) (off below half) -> **browsing** |
| Network busy | other traffic > max(512 KB/s, 15 % of the best speed) (off below half) -> **busy** |
| Slow down | after 3 readings in a row (`CONFIRM`): cap = reference x keep, never below the minimum; keep = mode share + 25 % (Aggressive 35 %, Balanced 50 %, Conservative 75 %); busy while away keeps half of the rest more |
| Settle | the 2 readings after the cap was set or lowered (`SETTLE`) change nothing: the system still takes in what was on its way to VeloxFetch's connections faster than the new cap reads it, which the counters show as someone else |
| Relearn | the reference and the best speed are learned again only 3 observations after a cap lifted (`RELEARN_AFTER`): the speed is still climbing back, and the buffered burst looked like a faster line |
| Speed up | after a calm (`CALM` 4 readings, 8 when the network got busy again soon after calming = unsteady): x1.5 per reading, full speed above 95 % of the reference |
| New mode / minimum | the cap follows at once (`retarget`), the controller keeps its state |
| No counters | full speed, the controller starts over (`unknown`) |

Status codes (`SmartThrottleDto.reason`): `off`, `fullIdle`, `fullFree`, `browsing`, `busy`, `app`, `easing`, `unknown`;
the UI turns them into the status line (`throttle.ts`, `thr.*` texts). The best speed and the reference
(`SmartThrottleDto.referenceBps`) are learned only while nothing caps downloads (the reference also not while the
controller is about to slow down). Idle time per OS: Windows `GetLastInputInfo`, macOS `CGEventSourceSecondsSinceLastEventType`, Linux
GNOME `org.gnome.Mutter.IdleMonitor` -> `org.freedesktop.ScreenSaver.GetSessionIdleTime` -> logind `IdleHint` /
`IdleSinceHint` (the way that answered is remembered).

Settings (saved): `smart_throttle.enabled` (off), `.auto` (on), `.min_bps` (256 KB/s; 32 KB/s to 1 GB/s), `.share`
(25 %), `.apps`, `.peak`. The maximum download speed is the global speed limit (shown in the same panel).

* Off by default (`smart_throttle.enabled`); the list and the share are saved settings. Default programs: popular
  games (League of Legends, VALORANT, CS2, Dota 2, Fortnite, Apex, Rocket League, GTA V, Overwatch, EA FC,
  eFootball, PUBG) and calls (Zoom, Teams). Discord is left out on purpose (often open all day); users can add it.
* Matching: whole names, no case, `.exe` ignored (`League of Legends.exe` = `League of Legends`).
* Commands: `get_smart_throttle`, `set_smart_throttle(request)` (`enabled`, `auto`, `minBps`, `sharePercent`, `apps`).
  UI: Settings > Speed > Smart throttle (switches, mode, minimum, maximum, programs, live status line and bandwidth
  graph), the dashboard card and the status bar.
* UI state: one copy for the whole window (`throttle.ts`, `throttleStore`), polled every 3 s; the card, Settings
  and the status bar read it. Changes go through `throttleStore.save`: shown at once, sent one after the other in
  order, no poll while one is on its way and a poll started before a change is dropped (the switch used to flick
  back, then forward); a refused change shows why and the real state comes back. Choosing a mode on the card while
  it is off turns it on in that mode. The gauge shows the cap against the reference ("25% now"; the mode's share
  while no speed is known). The card's core keeps its place while downloads are slowed (a CSS rule used to move
  it off the centre, its ring crossing the numbers); mode names shrink to fit a narrow card.
* Turning it off lifts the cap immediately (not at the next scan). Nothing about other programs is stored, logged
  or sent; only the matched name of a *listed* program appears in the log.

## Live preview

```text
<video src="http://velox-preview.localhost/12">            (Windows; Linux and macOS:
                                                            http://127.0.0.1:<port>/<secret>/12, below)
  -> src-tauri/src/preview_server.rs   main window only, GET / HEAD only, path = a download id only
  -> manager.read_preview(12, Range)   audio / video only; .part while downloading, the saved file once done;
                                       inside a download folder (AllowedRoots); only written bytes
  -> 206 Partial Content, Content-Range, Accept-Ranges, nosniff, CSP default-src 'none', no-store
```

* Only bytes that are really on disk are served: each unfinished segment holds back its last 512 KB (the writer may
  still have them in flight). On the custom protocol (Windows), a request for bytes not there yet waits up to 20 s
  while the download runs, then gets 503 + `Retry-After`. The private address (Linux, macOS) never answers 503 for a
  running download: the headers go out at once and the bytes follow as they are written (see below). Neither ever
  returns the zeros of an unwritten part.
* At most 2 MB per answer; the player asks for the next part. `bytes=a-`, `bytes=a-b`, `bytes=-n` (suffix) are
  understood; a start past the end is 416 with `bytes */total`.
* Types: mp4 / m4v / webm / mkv / mov / ogv / ts / avi, mp3 / m4a / aac / ogg / opus / wav / flac. Anything else
  (a web page, a program) is 415: the protocol can never load it into the window.
* `set_preview_mode(id, true)`: preview order (start, end, then in order); a running download is restarted once
  (pause + resume, nothing lost) so the end comes now. Turning it off never restarts anything. `get_preview_info(id)`:
  how much is ready from the start.
* UI: the details of an audio / video download have a **Preview** card ("Watch while downloading" / "Play").
* What a player can show depends on the file: MP4 with its index at the start, WebM, MP3 and WAV play from the first
  megabytes; an MP4 with its index at the end plays as soon as the end arrived (fetched second in preview mode);
  codecs the web view cannot decode (e.g. some MKV / AVI) do not play, though the download is unaffected.

### Linux: the private preview address (October 2026)

WebKitGTK's media player refuses every custom scheme (`GStreamerCommon.cpp isProtocolAllowed`: only http(s), file,
blob, data; the page got `FormatError` and nothing played). On Linux the same answers come from
`preview_server::loopback`:

* `reserve()` before the app is built: a free port on 127.0.0.1 and a 64-hex secret from `/dev/urandom`; that exact
  origin (`http://127.0.0.1:<port>`) is added to the CSP's `media-src` (`allow_in_csp`, production and dev policy;
  no wildcard, `connect-src` unchanged). `start()` serves it; `get_preview_base` (main window only) gives the page
  `http://127.0.0.1:<port>/<secret>/`, which `preview.ts` accepts only in exactly that shape.
* Rule 12.2 in full: 127.0.0.1 only; the secret in every path (else 404); `Host` must be that address (DNS rebinding:
  403); an `Origin` other than the app's is refused (403; a media element sends none); GET / HEAD only; ids only
  (`target_from_path`, as the custom protocol); request head at most 8 KB, read within 10 s; at most 16 connections
  and 40 new requests a second (429); no CORS headers; the same `read_preview` / `read_media_preview`.
* Answered like a web server serving a file still being written (`stream`): the full length the player asked for
  up front (`Content-Length`, `Content-Range bytes a-b/total`), then the bytes as the download writes them;
  answering only what was written made WebKit take that for the whole file (a 60 s song showed as 3.7 s and
  stopped). A video job whose size is not known yet is sent from the start without a length until it ends. A quick
  "not written yet" (nothing downloads it any more) or 120 s without progress ends the answer; the player asks again.
* Windows (WebView2) keeps the custom protocol, unchanged.

### Linux and macOS: why the preview stopped, and the fix (October 2026)

Symptom: on Linux the player loaded and played for a moment, then stopped for good. GStreamer's log said
`R2: Received 503 HTTP error code` (WebKit's second request). On macOS a downloading video did not play at all.
Windows was fine. Packages were not the cause: every GStreamer plugin was installed on the test machine.

Causes and fixes:

1. **A 503 for bytes not written yet.** After buffering, WebKit opens a second request at the point where it
   stopped. When those bytes were not written yet, `stream` waited 20 s and then answered 503. WebKit's source
   element treats that as fatal.

   Now a request for a download that is still running (`DownloadManager::preview_head`: media type, size, still
   running, the resolved start) is answered like any web server serving a growing file (`stream_growing`):
   * `206` / `200` with the full `Content-Length` and `Content-Range` at once;
   * then the bytes as they are written (`pump`), even if the first byte asked for is not there yet.

   A paused, finished or unknown download keeps the earlier answers (the saved file, `416`, `415`, `404`).
2. **macOS could not stream a growing file through the custom protocol.** WKWebView's player (AVFoundation) asks
   for exact byte ranges. It needs a file that is still being written to come as one answer of the full length,
   and a custom protocol's answer is one finished body. macOS now uses the same private address as Linux:
   * `#[cfg(any(target_os = "linux", target_os = "macos"))]` on `loopback`, its start-up and `get_preview_base`;
   * `/dev/urandom` exists there too;
   * `Info.plist` adds `NSAppTransportSecurity` → `NSAllowsLocalNetworking` (local addresses only; App Transport
     Security stays on for everything else, and `policy.test.mjs` forbids any wider exception).
3. **The file's start was not fetched first** once "Watch" was pressed on a running download, mostly under a speed
   limit. The playable start stayed at 0 for 20 s while 4 of 5 MB arrived, so the player waited for nothing.
   * A part that had to queue for a slot again went to the back of a first-come queue, behind the middle of the
     file. This happened when another connection's run had the part, when its connection gave way to a lower
     limit, or when it switched to short requests.
   * Every slot of the connection gate is now handed out by part order (`AdaptiveGate::acquire_ranked`; first tries
     through `acquire_in_turn` go through it too). A part's next short request also keeps its slot instead of
     queuing again (`GatePermit::is_leaving` / `rest`: a hold after refusals still applies).
   * Result: the start is playable about 2 s after "Watch" at 200 KB/s, and the first unfinished part never sits
     idle.
4. **The player recovers on its own.** If the player errors, `PreviewCard` reconnects it up to 3 times, 1.5 s
   apart. If it still cannot play (a format this computer's player does not decode), it says so plainly
   (`prev.cantPlay`) instead of showing a black box. The download is never affected.

**Installed with the app (Linux).** WebKitGTK plays media through GStreamer:

| Package | GStreamer packages |
|---|---|
| `.deb` | Recommends `gstreamer1.0-plugins-base` (playback, Ogg/Vorbis/Opus), `gstreamer1.0-plugins-good` (WAV, MP4/MOV, Matroska/WebM, FLAC, MP3, HTTP), `gstreamer1.0-libav` (H.264, AAC, HEVC...), `gstreamer1.0-plugins-bad` and a sound output (`gstreamer1.0-pulseaudio \| gstreamer1.0-pipewire`). Recommended, never required: `apt` installs them by default, and a system that cannot (pinned or mixed codec libraries) still installs the app. |
| `.rpm` | Recommends `gstreamer1-plugins-base`, `gstreamer1-plugins-good`, `gstreamer1-plugin-libav`, `gstreamer1-plugins-bad-free` and `gstreamer1-plugin-openh264` (Fedora ships H.264 that way). |
| AppImage | `bundleMediaFramework: true`: GStreamer and an allow-list of software playback plugins travel inside the AppImage (`scripts/appimage-media.sh`), so it plays on a system without them. The machine's hardware plugins (VA-API, NVIDIA, QSV, V4L2, Vulkan...) are never carried: they crashed the AppImage on other computers. It keeps its own plugin cache (`media_env.rs`). |

All of this is in `tauri.release.linux.conf.json`. Windows (Media Foundation in WebView2) and macOS (AVFoundation)
decode with the system's own frameworks: nothing to install.

**What each system's player can decode** (the download itself is never affected):

| | Windows (WebView2) | macOS (WKWebView) | Linux (WebKitGTK + the packages above) |
|---|---|---|---|
| MP4 / M4V / MOV (H.264, AAC) | yes | yes | yes |
| HEVC / H.265 | only with the system's HEVC extension | yes | yes (libav) |
| WebM / Ogg (VP8/VP9/AV1, Opus/Vorbis) | yes | WebM VP9/Opus on recent macOS; Ogg no | yes |
| MKV / AVI | often (codec dependent) | no | yes |
| MP3 / M4A / AAC / WAV / FLAC | yes | yes | yes |

When a format cannot be decoded, the card says so after the retries.

## Tests

`limiter.rs` (cap applies to running throttles and lifts at once; waiting connections wake when a limit is lifted),
`disk.rs` (a range delivered in one chunk still pays the limit), `planner.rs` (preview order), `manager/preview.rs`
(range parsing, media types, never unwritten bytes), `manager/smart_throttle.rs` (matching, cap, list checks),
`src-tauri` (`preview_server` ids only; the real process scan lists this process and takes milliseconds),
`manager/smart_throttle.rs` automatic mode (confirmation, no flapping between the thresholds, step-by-step recovery,
unsteady network waits longer, idle / busy, minimum always kept, no counters = full speed), `system.rs` (idle time:
dry run, and a by-hand test on the real desktop),
`manager/smart_throttle.rs` measurement (same window for both counters, headers are not traffic, a speed change is
not someone else, first reading / long gap / counters going back, the cap is a share of today's speed, new settings
move the cap without lifting it, buffering swings cancel out, readings recorded on a real capped line are not
browsing, noise at high speed), `adaptive.rs` (a dip never cuts a few connections, a throttling server is probed less
and less often, useless probes back off, a 429 halves once), `limiter.rs` (a shared limit shows as waits, lowering a
limit never stops downloads, raising one speeds up without a stop), `disk.rs` (a slow connection reports progress
often), `media.rs` (only a real change of limit restarts a video job),
gaming policy: `manager/smart_throttle.rs` (`GameWatch`: on at once, off after two scans, flicker never flips it,
several games, failed scans kept 15 s then lifted; each policy's cap is never zero; unrelated programs never match),
`src-tauri` (a program that renamed its main thread is found by its executable's name; fails without it),
`smart_preview.rs` (pause: running downloads pause with progress kept, nothing new starts, a second game keeps it,
only what it paused resumes and finishes intact, the user's pauses before and during the game stay; the user's
queue pause and a limit changed during the game outlast it; a restart during a game leaves nothing held; low holds
the chosen speed, flicker never lifts it, switching policy mid-game holds / lets go at once, off lifts everything,
bad limits refused), `media.rs` (the pause policy pauses a running video and resumes it intact after the game; fails
without it), `smart_throttle.rs` (a listed path matches that executable only, bad paths refused), `src-tauri`
(a listed path finds this test by its full path and not one in another folder; paths are read only when listed;
the private preview address: secret, Host, Origin, methods, ids, rate limit, CSP origin added exactly, size parsing),
`preview.test.ts` (the address the player uses, only a well-formed private one),
`smart_queue.rs` (Low priority waits during a call), `throttle.test.ts` (status lines, saving
the policy), `tests/e2e/desktop-gaming.mjs` (17 checks, the real app with a stand-in game process),
`tests/integration/tests/smart_preview.rs` (10 tests through the real manager and engine; one slows a real download
while browsing and recovers, checks the minimum, the switches and their persistence; one flips through the modes
while capped: bytes every half second),
`tests/e2e/desktop-throttle.mjs` (the real app: a mode chosen while off turns it on, five quick clicks end where the
last one says and never flick back, the app's own process as "a game" caps a running download to the mode's share
of its real speed, the gauge, the core stays centred while slowed, every mode name fits, modes switched while
capped never stop the download, Settings and the card agree, off = full speed at once; screenshots
`throttle-hud-limiting-dark.png`, `throttle-settings-dark.png`),
`tests/e2e/desktop-preview.mjs` (real WebView2: a WAV plays while downloading through the protocol, seeking, CSP blocks
scripts; the real process scan finds a listed program, the status line, a held-back download, full speed at once),
policy tests "live preview: ..." and "smart throttle: ...".

## Download Profiles and Smart throttle (October 2026)

A Download Profile may set Smart throttle's automatic mode (`SmartState::profile`):

* `Some(true)` (Balanced) turns it on;
* `Some(false)` (Fast) turns the automatic mode off but leaves Smart throttle itself, and a gaming policy, as you
  set them;
* `None` leaves everything as you set it.

What is in force: `on() = enabled || profile == Some(true)`, `automatic() = profile.unwrap_or(auto)`. Every place
that used `enabled` / `auto` for behaviour now uses them. The DTO keeps `enabled` / `auto` as your switches and adds
`profileAdaptive`. A profile change is applied at once (`smart_profile_changed`). See `docs/download-profiles.md`.

