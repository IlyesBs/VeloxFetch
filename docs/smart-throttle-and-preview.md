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
    sysinfo: process NAMES only (ProcessRefreshKind::nothing(), ~2-6 ms), on a blocking thread, 2 s limit
    + automatic mode: SystemIntegration::user_idle()   (time since the last input only; ~1-2 ms)
    -> manager.smart_throttle_observe_activity(names, idle)      timeout / panic -> observe(None) = full speed
download-manager/src/manager/smart_throttle.rs
    other traffic = computer's download counter (sysio, refreshed here even with the window hidden) - VeloxFetch's own
    listed program running?   yes -> cap = share of the best speed seen (at least the reserved minimum)
    else automatic mode?      Controller::step(other, idle, peak, share, minimum) -> cap or full speed, and why
    -> RateLimits::set_smart(cap)    (only when it changed; logged with the reason)
```

### Automatic mode (`Controller`)

| Signal | Rule |
|---|---|
| Smoothing | each reading moves the smoothed traffic half way (`SMOOTHING` 0.5) |
| Using the internet | the user is not away (idle < 2 min, or unknown) and other traffic > 48 KB/s (off below 24 KB/s) -> **browsing** |
| Network busy | other traffic > max(512 KB/s, 15 % of the best speed) (off below half) -> **busy** |
| Slow down | after 2 readings in a row (`CONFIRM`): cap = best speed x keep, never below the minimum; keep = mode share + 25 % (Aggressive 35 %, Balanced 50 %, Conservative 75 %); busy while away keeps half of the rest more |
| Speed up | after a calm (`CALM` 4 readings, 8 when the network got busy again soon after calming = unsteady): x1.5 per reading, full speed above 95 % of the best speed |
| No counters | full speed, the controller starts over (`unknown`) |

Status codes (`SmartThrottleDto.reason`): `off`, `fullIdle`, `fullFree`, `browsing`, `busy`, `app`, `easing`, `unknown`;
the UI turns them into the status line (`throttle.ts`, `thr.*` texts). The best speed is learned only while nothing
caps downloads. Idle time per OS: Windows `GetLastInputInfo`, macOS `CGEventSourceSecondsSinceLastEventType`, Linux
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
* Turning it off lifts the cap immediately (not at the next scan). Nothing about other programs is stored, logged
  or sent; only the matched name of a *listed* program appears in the log.

## Live preview

```text
<video src="http://velox-preview.localhost/12">            (velox-preview://localhost/12 on macOS / Linux)
  -> src-tauri/src/preview_server.rs   main window only, GET / HEAD only, path = a download id only
  -> manager.read_preview(12, Range)   audio / video only; .part while downloading, the saved file once done;
                                       inside a download folder (AllowedRoots); only written bytes
  -> 206 Partial Content, Content-Range, Accept-Ranges, nosniff, CSP default-src 'none', no-store
```

* Only bytes that are really on disk are served: each unfinished segment holds back its last 512 KB (the writer may
  still have them in flight). A request for bytes not there yet waits up to 20 s while the download runs (the player
  shows "loading"), then 503 + `Retry-After`; it never returns the zeros of an unwritten part.
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

## Tests

`limiter.rs` (cap applies to running throttles and lifts at once; waiting connections wake when a limit is lifted),
`disk.rs` (a range delivered in one chunk still pays the limit), `planner.rs` (preview order), `manager/preview.rs`
(range parsing, media types, never unwritten bytes), `manager/smart_throttle.rs` (matching, cap, list checks),
`src-tauri` (`preview_server` ids only; the real process scan lists this process and takes milliseconds),
`manager/smart_throttle.rs` automatic mode (confirmation, no flapping between the thresholds, step-by-step recovery,
unsteady network waits longer, idle / busy, minimum always kept, no counters = full speed), `system.rs` (idle time:
dry run, and a by-hand test on the real desktop),
`tests/integration/tests/smart_preview.rs` (5 tests through the real manager and engine; one slows a real download
while browsing and recovers, checks the minimum, the switches and their persistence),
`tests/e2e/desktop-preview.mjs` (real WebView2: a WAV plays while downloading through the protocol, seeking, CSP blocks
scripts; the real process scan finds a listed program, the status line, a held-back download, full speed at once),
policy tests "live preview: ..." and "smart throttle: ...".
