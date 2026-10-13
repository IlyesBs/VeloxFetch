# VeloxFetch architecture

> The UI is not the application. The product is a fast, reliable, secure Rust download core;
> the desktop shell is one client of it.

```
  React UI (TypeScript + Tailwind)             apps/desktop/src
        │  Tauri IPC (ids + validated DTOs only)
        ▼
  Tauri commands + per-window authorization    apps/desktop/src-tauri
        │
        ▼
  DownloadManager  (lifecycle, validation,     crates/download-manager
        │           events, queue/limits)
        ▼
  Downloader engine                            crates/downloader-core
   ├─ http-client   reqwest: probe, ranges     crates/http-client
   ├─ planner       segments + connections
   ├─ adaptive      connection controller
   ├─ limiter       token buckets
   ├─ scheduler     priority run-slots
   ├─ retry         backoff, classification
   ├─ integrity     SHA-256/512
   └─ disk          preallocation, bounded writes
        │
        ▼
  persistence (SQLite: downloads, segments,    crates/persistence
               settings, history)
  scheduler          time windows (pure logic) crates/scheduler
  system-integration power actions, browser host registration   crates/system-integration
  browser-bridge     protocol, validation, auth, local link     crates/browser-bridge
```

Browsers reach the app through a separate program, never directly (see *Browser integration*):

```
  Chrome / Edge / Brave / Firefox
        │  extension (apps/browser-extension)
        ▼
  velox-native-host   (apps/native-host, started by the browser; stdio)
        │  authenticated local pipe / socket
        ▼
  app: browser bridge ──► DownloadManager::create_download_from_browser
```

Rule: **React never touches the filesystem, network or OS directly.** It can only ask the Rust
side for a small, fixed set of things (see `apps/desktop/src-tauri/src/lib.rs`).

## Crates

| Crate | Role |
|---|---|
| `http-client` | Hardened reqwest wrapper. HEAD → `Range: bytes=0-0` discovery, *verified* range support, connection pooling, DNS cache, http(s)-only redirects. |
| `downloader-core` | The engine. Usable from the CLI, tests and the app without Tauri. |
| `persistence` | SQLite store (WAL, versioned migrations), crash recovery. Implements the engine's `DownloadStore` trait. |
| `download-manager` | Application controller the UI talks to. Owns create / start / pause / resume / cancel / retry / delete, the queue gate, the schedule and the power trigger, validates every input, emits events. |
| `scheduler` | `Schedule` (start/stop/days, overnight windows) and `queue_rules` (the Smart Download Queue's rules: time window, start time, idle, connection, busy line, big files): pure functions of a local timestamp and the readings passed in. No clock, no I/O. |
| `system-integration` | Clipboard text (`arboard`, all three OSes), default-browser detection and browser launching for the extension installer, `SystemIntegration` (Phase 15): the one door to the OS for power actions (shutdown / sleep / hibernate), show / open a file, keeping the computer awake while downloading, notifications, the `veloxfetch://` link protocol, idle time and the connection (online, Wi-Fi / cable / mobile, read from the routing table and the adapter, nothing sent), with `NativeSystem` per OS and `DryRunSystem` for tests (fixed commands, no shell); snap / Flatpak browsers; registration of the native messaging host with browsers (registry on Windows, manifest files elsewhere). |
| `media-engine` | Video streams (HLS `.m3u8`, DASH `.mpd`): manifest parsers, segment download with hard limits, optional ffmpeg remux. **A separate subsystem**: it shares only the HTTP client with the normal engine and nothing else (own job list, own events, own limits, no database rows). |
| `path-safety` | File-name sanitising and validation, "is this path really inside the download folder" (links resolved), and opening a file without following a planted link. Used by the engine, the media engine and the manager's validation so the rules exist once. |
| `secrets` | Sealing of small secrets (ChaCha20-Poly1305) with a key kept only in the OS credential store (Windows Credential Manager, macOS Keychain, Secret Service), an in-memory store for tests, and the log-redaction helpers. `persistence` seals the browser's cookies / credentials with it; there is no plaintext fallback. |
| `browser-bridge` | Everything the extension side and the app side share and nothing about downloading: length-prefixed frames with hard limits, the closed command set and its strict validation, caller (extension id) allowlist, mutual authentication, the local pipe / socket, the host loop and the server. |
| `apps/native-host` | `velox-native-host`: the small program browsers start (stdin / stdout native messaging). Validates, then forwards to the app. |
| `apps/browser-extension` | The Manifest V3 extension for Chromium browsers and Firefox (no dependencies, no content scripts). |
| `apps/cli` | `velox` command-line client (also the engine's manual test harness). |
| `apps/desktop` | Tauri v2 shell + React UI. |
| `tests/integration` | Fault-injecting local HTTP server + end-to-end tests of the layers above. |
| `tests/e2e` | Drives the real desktop app through WebView2 / WebKitGTK (IPC, CSP, shutdown, restart, the Smart Download Queue). |


## Download lifecycle

```
            create ──► queued ──► downloading ──► verifying ──► completed
                          ▲            │  ▲
                          │       pause│  │resume            failed ──retry──► queued
                          │            ▼  │                     ▲
                          └────────── paused ──── cancel ─► cancelled (partial data deleted, final)
```

* **Pause** keeps the `.part` file and all segment state. **Cancel** deletes them for good.
* State is checkpointed every 500 ms. Order matters: snapshot counters → `fsync` the `.part`
  file → commit to SQLite. A crash can lose recent progress but the database never claims
  bytes that are not on disk.
* On startup, anything left `downloading` is validated against its `.part` file and turned into
  `paused` (resumable) or reset to restart cleanly.
* Before resuming, the remote resource is re-validated (strong ETag, else `Last-Modified`, plus
  size). If it changed, the download restarts instead of splicing old and new bytes.
* **Resume integrity** (`docs/resume-integrity.md`):
  * every completed segment is hashed (SHA-256) by the checkpoint that first saves it complete, after the `fsync`,
    in the same commit, together with the `.part` file's stamp;
  * at resume, a different stamp (crash, outside change) re-hashes the completed segments, and a changed one is
    emptied and fetched again;
  * every ranged answer must be uncompressed, carry the download's strong ETag (when both have one) and the
    download's total in `Content-Range`, or it is refused (`source_changed` / `range_not_honored`);
  * the segment map must cover the file exactly, at resume and before finishing.

## Performance model

* The file is cut into ~4 segments per possible connection; a gate limits how many run at once.
* Runs of parts (October 2026): a connection's request carries its part and the untouched parts right after it
  (`Claims::run_after` in `engine.rs`), read one after the other from the same answer (`disk::pump_keep` keeps the
  bytes past a part's end for the next one). Runs are at most 16 parts and shrink as the work left shrinks (work
  left / twice the connections), so the last parts still spread over every connection. A part inside another
  connection's run waits for it (without a slot), and continues on its own if the run stops early. A request per
  part, on a server that limits requests (Hetzner), was refused again and again with seconds without a byte; now
  it is ~16 times fewer requests and no pause between parts.
* An adaptive controller starts on 4 connections (gentle with servers that cap connections per client)
  and climbs in big steps, 4 → 16 → 64, while extra connections still increase throughput (≥5 % gain);
  each step is judged one interval after it is taken, once the new connections are up to speed, and is
  reverted when it does not pay off. Later probes (every ~15 s) double. It halves on errors, on
  throughput collapsing for 3 intervals in a row, or on a slow first byte *together with* collapsed
  throughput (a slower first byte alone is normal with many connections). While a speed limit holds the
  download (a connection had to wait for a limit during the interval, or it runs at ≥80 % of the tightest
  limit: global, category, per download or Smart throttle), the controller changes nothing: a capped speed
  says nothing about the network. HTTP 429 / 503 reduce connections *immediately* and have their own retry
  budget.
* Who stops when the count goes down (issue #21, `docs/slow-servers.md`): the controller's own lowerings (no gain,
  a dip, errors, 429 / 503) are soft (`AdaptiveGate::set_limit_soft`). No new request goes over the limit, and a
  request still waiting for its answer gives way (`GatePermit::until_over_limit`, raced against the request with
  the answer first). A transfer checks `give_way_transferring`, which follows the `keep` count, lowered only by
  `set_limit` (a speed limit, the user's settings). Joins: a request ending before an unfinished part reads a 1 KiB
  tail. `check_joins` fetches what is left 4 at a time (`JOIN_LOOKS_AT_ONCE`). `run_sequential`: with a known size,
  a broken attempt continues (`continue_sequential`: `Range` + `If-Range`, Content-Range total, 1 KiB overlap),
  patiently for a server slow to answer (`HttpClient::get_range_patient`). It falls back to byte 0 once, for good,
  when the server cannot.
* A refused request ("too many connections") holds back the download's *new* requests: 1 s, then 2, 4… up to
  16 s while the server keeps refusing after each wait (`AdaptiveGate::server_refused`; a shorter `Retry-After`
  is taken, a longer one only for the refused part's own retry). Refusals that arrive during a wait are the same
  "no" and do not lengthen it. The connections already transferring go on. Before, a refused request freed its
  slot for the next part, refused too: up to 40 refusals in a row and seconds without a byte. One refusal after
  the count has worked for 20 s (`STABLE_FOR`) is a hiccup: the hold, but the count stays (halving 2 → 1 for one
  refusal halved the speed for minutes); right after a change, or twice within 20 s, it lowers as before.
* Steadiness (the speed must not swing between full and nearly nothing): a falling speed alone never cuts the
  connections below 4 (`COLLAPSE_FLOOR`; with few connections, fewer only means slower; errors still go lower);
  the retries that follow a 429 / 503 are not counted again as errors (that halved twice: 4 → 2 → 1 on a
  server allowing 2); every "no" (a throttling answer, a probe that brought nothing) doubles the wait before
  the next probe (15 s → 30 s → … → 8 min), so a server that limits connections is not hit again every 15 s;
  the best speed seen fades slowly so one lucky burst is not the reference for ever. Measured on a real
  server that allows 2 connections: before, the count cycled 4 → 2 → 1 → 2 → 4 every ~17 s and the speed
  between ~0.9 and ~2 MB/s; after, 2 connections for the whole run.
* Limits that change while downloads wait: any change settles what the connections owed the old limit (those
  bytes have already arrived). Lowered, the waits already running are kept; raised or lifted, the waiting
  connections go at once. (Kept in time, the debt still stopped every connection for 1.4 s the moment a cap went
  from 4.5 to 0.9 MB/s.)
* Connections under a speed limit (October 2026): a download held by a limit keeps only the connections the
  limit needs, 1.5 times what one connection gets while free, at least 2 (`capped_connections` in `engine.rs`;
  started under the limit, 2 until they fall short of it and one connection's speed is known), and all of them
  again the moment the limit lifts. The extra ones stop where they are at their next chunk
  (`GatePermit::give_way`, also while waiting for the limit: their unpaid bytes are handed back, `take_or_leave`).
  Each connection that stays then asks in short requests
  (about 4 s of its share, `Connection: close`, `AdaptiveGate::set_request_cap`), switching one after the other
  (closing all at once made servers refuse the new ones). Why: every connection's system receive buffer, grown
  at full speed, kept filling at line speed above the cap; the HTTP library then hands over up to 408 KB at
  once, and a limit paid after reading let the speed run 30–90 % over the cap for 10+ s, then made everyone wait
  for the backlog (3.5 s queues, "the download stops a while"). The line itself got bursts at full speed, which
  is what a game must not see, and the system's extra intake looked like someone else's traffic to Smart
  throttle. Measured on a real line, 800 KB/s cap: before, 14–22 connections, reads of 408 KB, a 7 MB burst
  when the cap lifted; after, 3–6 connections, reads of 30–100 KB, ~800 KB/s steady, no stop at the start.
* Bytes count (progress, speed) when they are written. A partly filled write buffer is written after 200 ms
  anyway, and under a speed limit a chunk is written and paid for in pieces of 64 KB (`PACED_PIECE`): at a low
  limit a full chunk took seconds to pay, so the progress stood still and then jumped.
* The speed shown for each download is smoothed over ~3 s, whatever the tick rate (the short pause between two
  parts of a connection no longer shows as a dip), and never above a limit it is held to.
* The dashboard's Network and Disk panels show real system counters (`manager/sysio.rs`, through `sysinfo`):
  the whole computer's download and upload speed (interfaces without a hardware address, such as loopback,
  tunnels and capture filter drivers, are skipped; each adapter counts once) and the read / write speed of
  the disk the download folder is on. Read at most about once a second, off the async threads; while no
  download runs the side panels ask for the metrics once a second, so the numbers stay live. A dash where
  the system gives no number.
* Bandwidth limits are token buckets: global + per-category + per-download, all changeable live.
* Memory per worker is bounded (`buffer × (channel + 2)`), independent of file size.
* Downloads beyond `max_concurrent_downloads` wait in priority order (Lowest … Highest).

## Queue, scheduler and power (Phase 6)

```
 user pause ─┐
             ├─► gate (open/closed) ─► SlotScheduler ─► max active downloads, priority order
 schedule ───┘                                          (each download: adaptive connections ≤ max connections)

 all downloads finished ─► [armed?] ─► visible countdown ─► SystemIntegration::power ─► disarmed (one-shot)
```

* **Max active downloads** is the scheduler's slot count and **max connections per download** is the
  engine's connection ceiling; both change at runtime and are saved. A new connection ceiling applies to
  downloads that start or resume afterwards.
* **Pause queue** closes the gate: nothing new starts and waiting downloads keep their place. With
  "also pause running", the downloads holding a slot are paused and remembered (`held`, persisted); they are
  the ones **Resume queue** brings back. Downloads the user paused or cancelled themselves are never resumed
  by the queue. Resumed downloads re-enter *ahead* of downloads that never started (`SlotScheduler::prefer`),
  and the gate only reopens after they have re-queued.
* **Scheduler**: "start at 22:00, stop at 07:00" on chosen days (a day is the day the window starts).
  Outside the window the gate is closed and running downloads are held; when it opens they resume. With a
  schedule turned on, queued downloads that were never started are started when the window opens (also at
  launch, if inside the window). The first evaluation happens in `DownloadManager::new`, before any command
  can race it. The manual pause and the schedule combine: the gate is open only if neither closes it.
* **Power actions** are off by default and only armed by an explicit request. Armed state lives in memory
  only, so a restart disarms it. It fires **once**, after every download has finished (a download the user
  paused counts as unfinished; downloads held by the queue too), following a countdown (60 s by default)
  that the UI shows on every screen and that can be cancelled. Quitting the app never triggers it.
  Windows tools are called by absolute path under `%SystemRoot%\System32`; nothing user-supplied reaches
  a command line.
* With nothing configured the manager behaves exactly as in Phase 5: the gate is open, a disabled schedule
  never closes it, no queue events are emitted for ordinary downloads (regression tests pin this).

Settings keys: `queue.max_active`, `queue.max_connections`, `queue.paused`, `queue.held`, `queue.schedule`.

## Smart Download Queue

Several queues with plain rules on top of the queue above (full description: `docs/smart-download-queue.md`).

```
 every second (queue loop, after the gate above)
   for each queue: rules (scheduler::queue_rules) on time · idle · connection · online · other traffic
        closed ─► its running downloads pause, remembered in queues.held   open ─► they resume
   start times that came ─► those downloads start ("start now": their queue's rules do not hold them)
   big files (size known, < 1/4 done) ─► the queue made for them
   SlotScheduler::set_policy(rank = queue order + place, blocked = downloads of closed queues)
 a download that starts ─► policy_for_new_run (held back at once if its queue is closed)
```

* **Slots:**
  - one `SlotScheduler` for everything;
  - a free slot goes to the highest priority, then (among equals) to what the queue resumed, then to the queue
    order and the place in the queue, then to the oldest;
  - blocked downloads keep waiting while the next one goes.
* **Express lane:** a High / Highest waiter that only waits behind less urgent running downloads gets one slot
  over the limit (never more than one). The manager turns it on; the scheduler's default is off.
* **The main queue (id 1)** has no rule unless the user gives it one. A download without a row in the `queue` table
  is in it, at place `id × 1024`, so the old order is kept.
* **Both layers must agree.** The global gate (pause, schedule window) and a queue's rules both have to allow a
  start. The queues never touch the global pause, the schedule or the power action.
* **Inputs:**
  - **Idle time:** asked from `SystemIntegration` only when a queue uses the rule.
  - **Connection:** read every ~5 s while something has to download, and again at once when a rule is saved.
  - **Other traffic:** the system's download counter minus the queue's own downloads, smoothed.
  - **No network:** confirmed by 2 readings. A download that is moving data proves the network works.
  - **Smart Throttle:** while it slows downloads (`smart_is_limiting`), Low / Lowest waiters are held back.
* **Events:** a `queues` event when anything a queue shows changes. `DownloadDto` carries `queueId`, `position`,
  `startAt`, `retrying` and `held`; ticks carry `retrying`.

Settings keys: `queues.list` (queues and rules), `queues.state` (paused, started by hand, used start time),
`queues.held`. Database v6: `queue.queue_id`, `queue.start_at`, `queue.pinned`.

## Download Intelligence

The read-only check before a download (full description: `docs/download-intelligence.md`).

* **`crates/download-manager/src/intelligence.rs`** holds the rules. They are pure: no I/O, the clock and the connection
  plan are passed in. `assess(Observed) -> AnalysisDto`, `expiring(url)`, `display_url(url)` and
  `find_duplicate(url, info, checksum, known)` (with a confidence, version comparison and `file_state`; see
  `docs/smart-duplicate-detection.md`).
* **`manager/intelligence.rs`** gathers the inputs:
  * `probe_quick` (`HEAD` + 1-byte range, no retry) under `ANALYSIS_TIMEOUT` = 20 s;
  * `fs4::available_space` on the nearest existing parent of `predicted_dir`;
  * `sysinfo::Disks` (`DiskRefreshKind::nothing()`, through `sysio::disk_for`), only for files ≥ 4 GiB;
  * `store.list()` plus `store.history(500)`;
  * for a finished match, one `metadata` call on its file (inside the download folders only), and `name_conflict`:
    whether the file name is taken in `predicted_dir` (a file, or an unfinished download's destination) and the
    numbered name it would get (`downloader_core::numbered_name`, the engine's rule).
* **`check_duplicate`** (Smart Duplicate Detection): the same matching for the Add bar, with no network request. The
  link is prepared as `create_download` stores it (credentials split, share links made direct). It returns
  `DuplicateDto | null`; the UI's `duplicates.ts` turns that into a prompt for each state. It is advisory: nothing
  existing is changed, and the engine's `claim_name` still keeps every copy in its own file.
* **`Downloader::connection_plan(size)`** is `planner::connection_limits` with the engine's current settings, so the
  recommended start is the engine's own; the adaptive controller still tunes it while the download runs.
* **Confirmation dialog:** `add_pending` spawns `analyze`, keeps its `AbortHandle` in the entry (`take_pending`
  aborts it) and fills `PendingDto.analysis`. The old `size` / `contentType` / `resumable` / `filename` fields come
  from the same answer.
* **Add dialog:** `analyze_link({ url })` runs one check at a time (`LinkCheck` in `Inner`); `url: null` cancels.
* **UI:** `components/IntelligencePanel.tsx` (rows, warnings, "Why?") and `src/intelligence.ts` (pure line building).

## Download Doctor

The diagnosis of a failure (full description: `docs/download-doctor.md`).

* **`crates/download-manager/src/doctor.rs`:** pure rules. `diagnose(&Evidence) -> Diagnosis`: category, source,
  retry class, severity, factors, actions. `Failure::from_code` maps the codes of `explain::error_code`.
* **`manager/doctor.rs`:**
  * `failure_of(&DownloadError)` reads the structured error, and the transport source chain only;
  * `failure_evidence` reads the run's events and retries, the record, the link (`intelligence::expiring`) and
    `system.network()` (only after DNS / connect / timeout failures);
  * `doctor_for` serves `DownloadDto.doctor` (the in-memory diagnosis, or one rebuilt from the error text after a
    restart, or a live one while retrying).
* **`manager/diag.rs` `note_end`:** diagnoses a failed run and saves the diagnosis in `DiagRecord.doctor`
  (`last_failure` tells a repeat). It keeps a copy in `Inner.doctor` and writes one timeline line before the
  failure line.
* **Recovery** is unchanged: the engine's `retry::with_retry` and `check_resumable`. The one change is in
  `http-client`: `is_retryable` refuses permanent TLS failures (`tls_will_fail_again`).
* **UI:** `components/DoctorPanel.tsx` and `src/doctor.ts` (pure lines and buttons), in `views/DetailView.tsx`.

## Browser integration (Phase 7)

```
 right click / popup ─► extension ─► browser native messaging ─► velox-native-host ─► local link ─► app
   validate (sender,     one-shot        registered per user,       validate again;       mutual auth,     validate a
   schema, URL)          message         only for our extension     start the app if      rate limit       third time,
                                                                    a download needs it                    then the manager
```

* **On the desktop (phases 16-18).** `apps/desktop/src-tauri/src/desktop.rs`: the tray icon and menu, closing to the tray, `--hidden`
  starts at login, notifications when a download ends while the window is not in front, and the show signal of a second launch. Its
  texts come from the UI's locale files, embedded at build time. The choices are stored by the manager (`manager/desktop.rs`), and
  start at login goes through `SystemIntegration::set_start_at_login` (Windows `Run` key, XDG autostart, macOS launch agent).
* **A download while the app is closed.** The helper starts the app for a `download` (never for a `ping`) and
  waits for its link. It finds the app by itself, never from a message (`browser_bridge::launch`): beside the
  helper (Windows, portable), `../../bin` (Linux packages), `../MacOS` (macOS bundle), else the path the app
  recorded in its data folder (`app-path`; an AppImage, whose helper is a copy in the data folder). A helper inside
  a snap / Flatpak cannot start programs: it asks the desktop portal to open `veloxfetch://start`, a link the app
  registers (Linux, while a sandboxed browser is connected). A second copy started meanwhile just leaves.
* **Isolation.** The downloader knows nothing about browsers: `browser-bridge` has no dependency on the
  engine, and the app turns a validated handoff into an ordinary `create_download`. The extension can only
  reach the native host, and the host can only reach the app's link.
* **What crosses over.** One of two commands: `ping` and `download` (URL, optional file name, referrer,
  cookies, user agent, private-window flag, source). Cookies / referrer / user agent travel to the origin as
  `Cookie` / `Referer` / `User-Agent` on every request of that download (probe, every range request), are
  kept with the record so a paused or restarted download still authenticates, and are **deleted when the
  download completes or is cancelled**. The UI's own IPC cannot set them.
* **The link.** A named pipe (Windows, first instance only, remote clients rejected) or a `0600` Unix
  socket. Each side proves knowledge of a per-run token (`bridge.token`, new every start) with HMAC over fresh
  nonces; the token itself never crosses the link, so a process squatting on the pipe learns nothing and an
  impostor server is refused by the host.
* **Starting the app.** Only for a `download` request, never for a `ping`: the host starts the executable that
  sits next to it (a fixed name, never a path from a message) and waits for it.
* **Registration.** Per user, only from the Settings button: Windows writes an `HKCU` key that points at a
  manifest in the app's data folder; Linux / macOS write the manifest into the browser's per-user folder.
  The manifest allows exactly the shipped extension ids. Brave, Vivaldi and other Chromium forks read the
  Chromium key on Windows (verified against a real Brave), so "Brave" registers there.
* **Extension ids.** The Chromium id is pinned by the public `key` in the extension's manifest and equals
  the id compiled into the host (a test compares them); the Firefox id is `velox-fetch@velox.app`.

## Batch downloads and clipboard (Phase 8)

```text
links.txt / pasted list  --                             >-- text_lines / expand_pattern --> classify (validate, normalise, dedupe) --> preview_batch (nothing added)
pattern file_[001-100].zip -/                                                                               add_batch (create_download per good link)

clipboard (opt-in) --> ClipboardSource (arboard | test file) --> detect_download_links --> notice --> user --> review_link (details + Start/Cancel)
                                                                                                         \--> batch dialog (several links)
```

All of the parsing, limits, duplicate detection and validation is plain Rust in `download-manager::batch` (no OS-specific code). The
only OS-specific piece is reading the clipboard, behind the `ClipboardSource` trait in `system-integration`. See `docs/security.md`.

## Video streams (Phase 9)

```text
Normal download engine                     Media engine (separate subsystem)
  HTTP files, Range downloads                HLS  (.m3u8): master / variant playlists, segments, init segments (EXT-X-MAP),
  SQLite rows, queue, power trigger                 byte ranges, discontinuities, audio + subtitle renditions
                                             DASH (.mpd):  Period / AdaptationSet / Representation, SegmentTemplate (number, time, timeline),
                                                           SegmentList, single-file BaseURL, initialization + media segments
                                             Remux: optional ffmpeg, arguments as an array, never a shell string
```

```text
link --analyze_media--> qualities / audio / subtitles (nothing written)
     --start_media(ids)--> re-read manifest --> scratch folder (segments, 6 at a time, in order) --> assemble --> [ffmpeg -c copy] --> final file(s)
```

* `crates/media-engine`: `hls.rs` and `dash.rs` (pure parsers, no I/O), `download.rs` (fetch, assemble, names), `ffmpeg.rs` (find + run). Everything but `ffmpeg.rs` and the HTTP client is identical on every OS; `ffmpeg.rs` differs only by `CREATE_NO_WINDOW` on Windows and an executable-bit check on Unix.
* `scripts/security-gate.mjs` and `tests/security/policy.test.mjs` are the Phase 12 gate: they read the repository and the tools' output, so they run the same on every OS and in CI (`.github/workflows/ci.yml`, `build.yml`). Nothing in them is Windows-specific.
* `crates/secrets`: pure Rust (`keyring` picks the store per OS: Windows Credential Manager, macOS Keychain, the Secret Service over D-Bus without C libraries on Linux). Compile-checked for Linux, macOS and Windows; the real store round trip is tested on Windows with `VELOX_TEST_OS_KEYRING=1`.
* `crates/path-safety`: pure `std` (plus `libc::O_NOFOLLOW` on Unix); the rules are the strictest of the three OSes, so a saved name is valid wherever the file is later copied. Link tests use folder junctions on Windows (no privilege needed) and symlinks on Unix.
* `crates/download-manager/src/manager/media.rs`: the job list (in memory, at most 3 running, 100 kept), progress events (`media`, `mediaRemoved`), the shutdown hook, and the "busy" flag the power trigger honours.
* The UI sends a link plus **ids** from the analysis; the manager re-reads the stream and refuses ids it does not know. The UI never supplies a segment address, a path, or anything for a command line.
* What is saved: the file type the user picks (`format`: MP4, MKV or WebM; a closed list checked in Rust). With ffmpeg, video + audio become one file of that type (MP4 by default; tracks that cannot go into the chosen type go into `.mkv`, with a note) and a lone transport stream becomes `.mp4`; without ffmpeg the tracks stay separate (`name.ts` / `name.mp4`, `name.audio.m4a`), with a note explaining why. Subtitles are always separate `.vtt` files. If ffmpeg fails, nothing is lost: the tracks are kept and a note says so.
* Not supported, refused with a plain reason: encrypted streams (AES-128, SAMPLE-AES), DRM (`ContentProtection`), live streams (no `ENDLIST` / `type="dynamic"`), DASH with several periods, an HLS stream that changes its initialization segment part-way. Stream jobs are not saved across a restart (a finished file stays on disk).
* ffmpeg: the one VeloxFetch fetched for itself (Settings > Video tools) first, then next to the app, then in the `PATH`. A stream that needs joining waits for it when it is being fetched; without it the tracks are kept as they are.
* Video pages (yt-dlp): the user picks the quality and the file type. MP4 prefers H.264 + AAC at the chosen height (plays everywhere) and is joined / repackaged into `.mp4` without re-encoding (`--merge-output-format mp4 --remux-video mp4`); MKV keeps the best picture and sound; WebM prefers VP9 + Opus and converts only what cannot go into WebM; MP3 / M4A / Opus keep only the sound (`--extract-audio`). Every value comes from a fixed list in `site.rs`, never from the UI.
* Video progress: the bar is shared by real weight (`site.rs` `shares`): the picture is ~88 % of the download, the sound the rest,
  and joining / converting gets the end of the bar (4 % for a copy, 15 % for an MP3 / WebM conversion). ffmpeg writes how far it is
  to `ffmpeg-progress.txt` in the job's work folder (`--postprocessor-args "<step>+ffmpeg_i1:-progress <file>"`), read twice a second
  against the video's length (`%(info.duration)s` in the progress lines), so the bar keeps moving while it converts.
* Speed limits apply to video jobs too: stream pieces pass through the same token buckets as ordinary downloads (`media_pacer`), and
  yt-dlp gets the tightest of the global limit and the Smart throttle cap, shared between running video pages, as `--limit-rate`
  (`site_rate`). When that limit changes by more than a quarter while yt-dlp downloads, it is restarted with the new one and
  continues its partial files. The Smart throttle learns the line's peak from ordinary downloads and video jobs together.

## UI performance model (60 FPS)

```
 engine ─► metrics aggregator ─► one batched "tick" ─► store (1 flush / frame) ─► React
            (download-manager)    5 per second          structural sharing        memoised cards
```

* The manager **never forwards individual engine updates**. Every 200 ms it samples all running
  downloads and emits one `tick` with each download's progress plus global metrics (speed, active,
  queued, lifetime and session bytes, disk space). It goes quiet when nothing is running.
* The frontend store queues events and applies them together once per animation frame; a
  download's object keeps its identity unless one of its values changed, so untouched cards do not
  re-render. Progress bars animate with `transform` only.
* The segment monitor is a canvas: detail data arrives 4×/s and each chunk eases toward its latest
  value in a `requestAnimationFrame` loop that stops when everything has settled.
* Measured in the real app (`tests/e2e/desktop-ui.mjs`): 60 fps, p95 frame 16.8 ms, no long tasks,
  ~50 DOM mutations/s, 5.0 ticks/s while downloading.
* Paint cost (details and numbers in `docs/ui-performance.md`): on Linux the web view paints on the CPU, so the
  styles there carry no box shadows or drop-shadow filters (rings become outlines, the glass highlight a top border);
  the window-wide backdrop is a picture drawn once (`aurora.ts`) on every platform. Six running downloads: 23 -> 62
  fps on a real screen, 22 -> 62 on a virtual display. `tests/e2e/desktop-perf.mjs` measures it.
* No flash when lists change (issue #20): an item's entrance animation plays once (`ui/entrance.ts`), so cards that
  React moves or re-creates (a download starting, a card changing section) never fade out and in again; the
  connection grid's pulse changes pace through `playbackRate`, never its duration (`ThreadMatrix.retime`).
* Start-up: the window is created hidden and shown from a layout effect (at commit, ~60 ms of page time; a passive
  effect was held back to ~310 ms in the hidden window), over a background in the interface's theme colour.

## Design system

The layout follows the reference design `docs/design/neon-dashboard.png`. Tokens live in
`apps/desktop/src/styles.css`: "Cyber Obsidian" (dark, retuned to the reference: deep navy `#050A19`, cards
`rgba(9,17,36,.62)`, a neon gradient cyan `#00E1F5` -> royal blue `#1857EB` -> violet `#9B1CF5`) and "Nordic
Alabaster" (light: `#F4F6F9`, royal blue `#3A86FF` -> `#5B6CFF` -> violet `#7B2CBF`), light / dark / system,
tabular numerals, reduced-motion support. Components never hard-code colours.

* **Window frame** (`lib.rs` `window_chrome`, `src/chrome.ts`): the UI draws its own title bar
  (`components/shell/TitleBar.tsx`, `data-tauri-drag-region`). Windows 11: no system title bar, Mica behind a
  transparent page (`data-surface="mica"`; the page keeps a theme tint for contrast); Windows 10 and Linux: an
  opaque "solid slate" window with no blur (`solid`; Linux gets GNOME-style round window buttons); macOS: native
  traffic lights over the bar (`glass`, no private APIs, so no window transparency; frosted cards over the
  aurora). The frame's theme follows the app's (`setTheme`). Windows 11 is detected with `RtlGetVersion`.
* **Shell** (`App.tsx`): a full-height sidebar (`shell/Sidebar.tsx`: the neon mark, Downloads / Active / Queue /
  Completed / History / Settings with magnetic icons and a glowing edge, and the Smart throttle card), the title bar
  beside it (`shell/TitleBar.tsx`: brand, version pill, language menu, theme button, window buttons) and a status
  bar across the window (`shell/StatusBar.tsx`). Every sidebar row has its own view: "Active" (`views/ActiveView.tsx`:
  running / waiting / paused work with live speed, connections in use, time left and bytes still to come) and "Completed"
  (`views/CompletedView.tsx`: finished files with search, kinds, open / show in folder / remove / delete from disk, which
  always asks first through `components/DeleteConfirm.tsx`).
* **Dashboard** (`views/DownloadsView.tsx`): the link bar (paste button; the Add Download split button holds
  Several / Video / Options), six stat cards (`BentoCards.tsx`), the tabbed list (tabs with counts, search,
  detailed / compact view, sort) and a side column (`SidePanels.tsx`: network and disk charts, upcoming
  downloads, quick actions). Only real numbers are shown: the Network and Disk panels read system counters (see
  *Performance model*).
* **Smart throttle** (`ThrottleHud.tsx`): a gradient radial gauge whose core is the on/off button (the arc shows the
  share of the connection downloads may use right now), a switch, three modes (Aggressive / Balanced / Conservative)
  and the live status line. `throttle.ts` polls the state every 3 s, keeps the speed / limit history for the graph in
  Settings (`SmartThrottleSettings.tsx`, `BandwidthGraph`) and, while it is on, sets `html[data-throttle]`
  (`limiting` while downloads are slowed), which turns the accent amber everywhere.
* **Task cards** (`DownloadCard.tsx`): the file's kind as a big thumbnail with its extension, name, size and
  link, the neon progress track (green when finished) with the percent, live numbers, the 8x8 connection panel
  (`ThreadMatrix.tsx`: one cell per connection slot, lit cells flicker in CSS at a rate set by the
  per-connection throughput) and round buttons (pause / resume, cancel, folder, menu).
* **Responsive** (no horizontal scrolling from 640 x 480 up): the sidebar folds to an icon rail under 900 px (or
  by the user; the Smart throttle card then moves into the page); stat cards take 2 / 3 / 6 columns, the side
  column moves under the list below 1040 px of page, and task cards drop the connection panel, then stack their
  buttons, as they narrow (container queries). The window opens at 1360 x 860 but never larger than 92 % of the
  screen's work area (`fit_to_screen` in `lib.rs`). Layout choices (folded sidebar, density, sort) are kept in
  `prefs.ts` (localStorage `velox.ui`, cosmetic only).
* **Languages** (`src/i18n.ts`): English, French, Arabic. `<html lang dir>`; layouts use logical sides
  (`ms/me/ps/pe/start/end`) so Arabic mirrors the whole window; Arabic uses the bundled Cairo font
  (`@fontsource-variable/cairo`, loaded only for Arabic glyphs, never letter-spaced); values inside Arabic
  sentences are bidi isolates. The shell, dashboard, cards and lists are translated; deeper Settings panels
  and dialogs are still English.

Views: Downloads (bento metrics + cards), Queue, History, Settings, Download detail
(overview, network, controls, segment monitor, metadata). Screenshots: `docs/screenshots/`
(`redesign-*.png`, `responsive-<width>x<height>.png` for six window sizes, `window-mica-*.png` for the real window with Mica).

* `crates/failpoint` (Phase 13) is the crash-test hook: `failpoint::hit("name")` calls sit in the engine, the disk writer and the SQLite store, compile to nothing in shipped builds,
  and let `tests/integration/tests/crash.rs` kill a real worker process at exact points. See `docs/testing.md`.

* **Where downloads go** (`crates/download-manager/src/manager/storage.rs`): the default folder (changeable), folders picked for single downloads, sorting by type
  (`downloader_core::filetype` decides the kind from the extension, then the `Content-Type`). The engine asks a `Placement` hook for the folder once the final file name is known;
  the set of folders it may write under is a shared `path_safety::AllowedRoots`. Settings keys: `storage.root`, `storage.allowed`, `storage.sortable`, `organize.enabled`, `organize.names`.

* **Network settings** (`manager/network.rs`): proxy mode / address / sealed password and the user agent. A change builds a
  new `HttpClient` and swaps it into the engine (`Downloader::set_client`) and the stream downloader; running transfers keep
  the client they started with. Settings keys `network.*`.
* **Finished files** (`manager/files.rs`): show in folder / open (through `SystemHandle`, a dry run in tests), history
  removal (`SqliteStore::forget_history`, which keeps the lifetime total in `stats.forgotten_bytes`), sign in and retry,
  speed limits by kind (a download without its own category is throttled under its kind's key, e.g. `video`).
* **Names**: the engine reserves the final name when a download starts (`Downloader::claim_name`): the first of `name`,
  `name (1)`, ... that no file, no running download and no unfinished download in the store (`DownloadStore::destination_in_use`)
  uses. At the end it never replaces a file that appeared meanwhile; it takes the next free name.
* **Errors** (`explain.rs`): `DownloadDto.error` is a plain sentence, `errorDetail` the engine's message, `needs` what only the
  user can fix (`signIn`, `proxySignIn`, `space`, `folder`); `kind` is the file's kind (icons, limits, sorting).

* **Diagnostics** (Phase 14, `crates/diagnostics` + `manager/diag.rs`): the engine records per run the last HTTP status, the
  retries (server check and segments), the most connections at once and a short list of events (`ProgressHandle::events`);
  when a run ends the manager adds them to the download's saved diagnostics (JSON in table `diagnostics`), writes one
  `download ended` log line and emits the usual update. The desktop app installs `LogHandle` (a small `tracing` subscriber: level
  filter, span context, redaction, memory ring, rotating files, crash hook) and passes it to the manager in `ManagerConfig.log`.
  Commands: `get_download_diagnostics`, `get_logs`, `set_detailed_logging`, `get_diagnostics_report`, `open_log_folder`.

* **Smart throttle and Live preview** (`docs/smart-throttle-and-preview.md`): `manager/smart_throttle.rs` decides a cap
  that goes into the `smart` bucket of `RateLimits` (a listed program's gaming policy, or the automatic `Controller` fed by
  the computer's other traffic from `sysio` and the user's idle time); `src-tauri/src/smart_throttle.rs` feeds it the
  running program and executable names and `SystemIntegration::user_idle`. Gaming policy: `GameWatch` debounces
  detection (on at once, off after two scans, 15 s through failed scans); *adaptive* = the mode's share, *low* = a fixed
  limit, *pause* = the queue gate closes (`QueueState::game_hold`, in memory) through `sync_game_hold`, which reuses the
  queue's `hold_running` / `release_held`, so only what the game paused resumes. Gate = not user-paused and in the
  schedule window and not held for a game. `manager/preview.rs` reads the written bytes of a download for `src-tauri/src/preview_server.rs`, a Tauri v2
  asynchronous custom protocol (`velox-preview`); `DownloadControl::set_preview` makes the engine start the last
  segment second, and the gate hands out every slot by part order (`AdaptiveGate::acquire_ranked`; first tries via
  `acquire_in_turn`), so a part that queues again keeps its place and the start of a watched video never waits behind
  the middle. A part's next short request keeps its slot. On Linux and macOS (WebKitGTK cannot play from a custom
  protocol; WKWebView cannot stream a growing file through one) the same reader is served on a private 127.0.0.1
  address (`preview_server::loopback`, rule 12.2), whose exact origin joins `media-src` at start-up. A running
  download is answered as a growing file (`preview_head`, `stream_growing`): headers at once, bytes as written. The speed shown per
  download is smoothed but never above the limits it is held to (`shown_speed`).

## State and versioning

`PRAGMA user_version` tracks schema migrations (currently v7: v4 request headers handed over by browsers, v5 per-download diagnostics, v6 the Smart Download Queue's places, v7 segment hashes and the `.part` stamp). A database from a newer build is
refused rather than corrupted.

## Download Profiles

Presets over existing settings (full description: `docs/download-profiles.md`).

* **`crates/download-manager/src/profiles.rs`** holds the pure rules:
  * the built-ins (`default`, `fast`, `balanced`, `lowResource`);
  * `resolve(base, profile)`: a profile's value where it has one, the user's own otherwise;
  * validation, the saved format (`Saved`, one `profiles` setting) and its lifecycle.
* **`manager/profiles.rs`**: `ProfilesState` (saved profiles, the user's global limit, the last applied values) and
  `apply_profile`, the only place profile values are put in force:
  * `RateLimits::set_global`;
  * `SlotScheduler::set_max_slots`;
  * `Downloader::set_max_connections`;
  * `SmartState::set_profile`.

  `set_speed_limit(Global)` and `set_queue_config` now store the user's own value and call `apply_profile`.
  Smart throttle reads `on()` / `automatic()`.
* **Engine:** `spawn_sampler` reads the connection ceiling (`Downloader::max_connections`) every tick and calls
  `AdaptiveController::set_ceiling`, so running transfers follow a changed setting without a restart.
* **IPC:** `get_profiles`, `profile_action` (main window only). Event `profiles`.

## Download again

Full description: `docs/redownload.md`.

* **`DownloadManager::redownload(id)`** (`manager.rs`):
  * accepts completed, cancelled or failed;
  * deletes the partial file inside the allowed folders;
  * resets the record to "not started": the destination becomes the folder, and size, parts, progress, version
    tags and error are cleared;
  * then calls `start_download`.

  The engine then plans it as a new download (`is_unstarted` path), and `claim_name` gives a free name if the file
  is still there. No engine change.
* **`DownloadDto::file_missing`**: a completed download with nothing at its path. Computed in `to_dto`.
* **UI:**
  * `redownload.ts` (where it is offered);
  * `PrimaryAction` / `downloadMenu` in `DownloadCard.tsx`, and `CompletedView`;
  * `bootstrap` also resyncs on window focus (throttled to 3 s).
* **IPC:** `redownload` (main window only).

## Use a new link

Full description: `docs/new-link-recovery.md`.

* **Rules** (`crates/download-manager/src/relink.rs`, pure): `identify` (verdict + evidence from the stored size /
  validators / content type, the new link's probe and the byte samples), `decide` (auto / confirm, `noRanges` and
  `noValidator` blockers, nothing-to-reuse), `sample_windows` (≤ 2 × 64 KiB inside checkpointed bytes), `headers_for`
  (same-origin credentials only), `same_source`.
* **Flow** (`manager/relink.rs`, `DownloadManager::use_new_link`):
  1. validate the link (as when adding: credentials out of the link, direct-link rewrite);
  2. check the download is paused / failed and not running, and claim it (`Inner::relinking`);
  3. refuse the same link and session;
  4. `probe_quick` with a 20 s limit, then `Downloader::sample_range` against the `.part` file (read only inside
     `AllowedRoots`);
  5. decide;
  6. switch in one store step only if the status and segment progress did not change meanwhile (link, sealed
     headers, the new link's strong ETag / Last-Modified);
  7. `start_download`.

  A pending browser request (`pending_source`) supplies its link and headers and is dismissed only when used.
* **Engine:** one addition, `Downloader::sample_range` (one ranged GET, `206` + `Content-Range` checked by
  `get_range_with`, at most `len` bytes read). Resume, `If-Range`, segment writes and final verification are
  unchanged and do the rest.
* **UI:**
  * `newLink.ts` (where it is offered, what an outcome says);
  * `NewLinkDialog.tsx` (`NewLinkHost` mounted once in `App`, opened by the `velox:new-link` event from the card
    menu and `DoctorPanel`);
  * `PendingDialog.tsx` (continue a stopped duplicate with the browser's link).
* **IPC:** `use_new_link` (main window only).

## Video download details

Full description: `docs/video-details.md`.

* **`MediaJobDto`** gains:
  * `url`, `quality`, `format`, `audioOnly`, `subtitles`;
  * a clock (`startedAtMs`, `endedAtMs`, `activeMs`, `runningSinceMs`, kept by `track_time` on every change);
  * `fileSize`, `retries`.
* **UI:**
  * route `media`, opened by the `velox:open-media` event from `MediaJobs`;
  * `MediaDetailView` reuses the file details page's `Card` / `Stat` / `Meta`;
  * `MediaControls` and `MediaPlayer` are shared with the row.
* No new command.

## Drop-down lists

`apps/desktop/src/ui/selectMenu.ts`, installed in `main.tsx`: every native `<select>` opens an app-drawn, themed
list in place of the system one, which no CSS can colour. The value is set through the native setter, followed by
a real `change` event, so components are unchanged. Details: `docs/ui-performance.md`.

## Linux media packaging

* `.deb` / `.rpm`: the system's GStreamer plays the preview. Its packages are recommended, not required
  (`tauri.release.linux.conf.json`); a format nobody can decode shows "can't play" on the card.
* AppImage: `scripts/build-release.sh` sources `scripts/appimage-media.sh`, stages the allow-listed plugins into
  `target/appimage-gstreamer-plugins` and passes that folder to linuxdeploy (`GSTREAMER_PLUGINS_DIR`).
* At start, `media_env::prepare` (first call of `run`, before any thread) points the AppImage's GStreamer at its own
  cache file. Inherited by the web view's processes. Details: `docs/release-process.md`.

## Sign-in in the browser confirmation

`PendingDialog.tsx` has a "Sign in to this site" part (user name, password), open by itself when the pending
download's analysis says `access: authRequired`. `api.confirmPendingDownload(..., login)` sends them in
`ConfirmPending` (`username`, `password`); `manager/pending.rs::confirm_pending` validates them before taking the
request, then sets them on the `NewDownload`, and `create_inner` seals them like the Add dialog's. Without a sign-in a
protected download fails with `needs: signIn`, and its card and details page still offer **Sign in…**.

## Updates (Phase 19)

```
lib.rs update_config()  --UpdateConfig (keys from update-keys.txt, fixed feed, kind)-->  DownloadManager
manager/update.rs       check_for_updates -> update_verify::{verify_manifest, decide} -> state + ManagerEvent::Update
                        download_update   -> <app data>/updates/<name>.part -> check_file -> rename  ("ready")
                        prepare_update_install -> check_file again -> UpdatePlan
lib.rs install_update   -> system_integration::update::install(kind, path, size, sha256)  (checks again)
                        -> Restart: app.exit(0) (normal shutdown)  |  Opened: the system's installer
```

* `crates/update-verify`: pure checks (no network, no writes): trusted keys, the signed manifest, versions, the
  package for a target, a file's exact size and SHA-256. Used by the manager, `system-integration` and its tests.
* `download-manager/src/manager/update.rs`: the state machine (`idle`, `checking`, `upToDate`, `available`,
  `elsewhere`, `downloading`, `ready`, `failed`), the background check (60 s after start, then every 12 h, when the
  setting `update.auto` is on), its own HTTP client (the user's proxy, no cookies, no extra headers).
* `system-integration/src/update.rs`: the installation kind (`installation_kind`), the install per system
  (`run_windows_setup` with a read-only-sharing handle, `replace_file` for the AppImage, the system's opener for
  `.deb` / `.rpm` / `.dmg`), the release page. A relaunched AppImage gets `--after-update` and waits (up to 60 s) in
  `lib.rs` for the old copy to free the browser link before taking over.
* IPC (main window only): `get_update_status`, `check_for_updates`, `download_update`, `install_update`,
  `set_update_auto`, `open_release_page`. None takes an address, a path or a version.
* UI: `components/UpdateSettings.tsx` (Settings > Updates), the status bar item, `State.update` fed by the `update`
  event. Text in `locales/*.json` (`upd.*`, `foot.update*`).
* Release side: `scripts/update-key.mjs` (`generate`, `manifest`, `sign`, `verify`). Details: `docs/auto-update.md`.

## Performance measurement (Phase 20)

* `tests/bench` (`velox-bench`, a workspace member, never shipped): `run` starts `serve` (a range-capable HTTP/1.1
  server with a computed pattern, optional per-connection cap) as a child process, then each case as a `case` child
  process (the real `Downloader` + `SqliteStore` wrapped in a timed `DownloadStore`); `db` times `SqliteStore::save`.
* `downloader-core/src/verify.rs` `RunningHashes` / `hash_progressive`: segment hashes continued at each checkpoint and
  kept for the run, held by the `ProgressHandle` (`resets` counter, emptied by `init` and `reset_segment`).
* `tests/e2e/desktop-perf.mjs`: start-up, frame rate, IPC latency. Numbers: `docs/benchmarks.md`.
