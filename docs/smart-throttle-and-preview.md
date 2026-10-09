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
shows e.g. `MainThread`) and is not cut at 15 bytes; its folder is never kept.

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
`release_held` (only what is still paused, at the front of the queue). The hold itself is in memory only: after a
restart nothing is held, the remembered downloads resume at launch, and a game still running is seen again within
3 s. A download the user pauses during the game is taken off `held` (`forget_held`) and stays paused; one the user
starts during the game waits for the gate; one added during the game starts after it.

**Shown**: the card, the status bar and Settings say "{app} is running · limited to {speed}" (adaptive, low) or
"{app} is running · downloads paused until it closes" (pause), only while a program is really detected; the Queue view
says "Paused while a game or call runs · N will resume when it closes". No notification.

**Limits**: names only, no full executable paths in the list; launchers whose game runs under another name need
that name in the list; in pause, video jobs are not paused but held to the reserved minimum; another user's or a
protected process shows its name only (its executable is not readable), which is enough for matching.

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

### Automatic mode (`Controller`)

| Signal | Rule |
|---|---|
| Smoothing | the last two (signed) readings are averaged, then each reading moves the smoothed traffic half way (`SMOOTHING` 0.5); it may go below zero by at most max(96 KB/s, 10 % of VeloxFetch's speed) |
| Noise | of VeloxFetch's own speed, 3 % (30 % while a limit paces downloads: `paced`) never counts as someone else |
| Using the internet | the user is not away (idle < 2 min, or unknown) and other traffic > max(96 KB/s, noise) (off below half) -> **browsing** |
| Network busy | other traffic > max(512 KB/s, 15 % of the best speed) (off below half) -> **busy** |
| Slow down | after 2 readings in a row (`CONFIRM`): cap = reference x keep, never below the minimum; keep = mode share + 25 % (Aggressive 35 %, Balanced 50 %, Conservative 75 %); busy while away keeps half of the rest more |
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
bad limits refused), `smart_queue.rs` (Low priority waits during a call), `throttle.test.ts` (status lines, saving
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
