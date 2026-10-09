# Testing strategy (Phase 13)

One command runs everything that can run without a browser or a display: `node scripts/security-gate.mjs`
(formatting, clippy, every test below, the policy checks, the audits). One command for the tests alone:
`cargo test --workspace --locked`, then `npm test --prefix apps/desktop`, `npm test --prefix apps/browser-extension`,
`node --test tests/security/policy.test.mjs`. The end-to-end scripts in `tests/e2e/` drive the real desktop app
(see "End to end" at the bottom).

## What each roadmap item is tested by

### 13.1 Unit tests

| Roadmap item | Where | What it proves |
|---|---|---|
| Chunk calculations | `downloader-core/src/planner.rs` | size table, limits, and a **property test over 20 000 random sizes / settings** (1 byte to 16 TiB, limit 0..70, minimum 0..8 MiB): the segments always cover exactly `[0, size)`, no gap, no overlap, never empty, never more connections than the caller allowed |
| Range calculations | `planner.rs` (`resume_offsets_never_leave_their_segment`), `http-client` (`content_range`, `content_range_arithmetic_cannot_overflow`) | a resume asks for `next_offset..=end` inside its own segment for any progress; `Content-Range` parsing survives overflow and junk |
| Filename parsing | `downloader-core/src/filename.rs`, `path-safety` (20 000-case property test), `http-client` (`disposition`) | Content-Disposition (RFC 6266 / 5987), traversal, reserved names |
| Header parsing | `http-client` (`robustness::hostile_header_values_never_crash_the_parsers`, `extras_*`, `retry_after_accepts_seconds_and_http_dates_and_nothing_else`) | hostile values, the allow-list of forwarded headers, `Retry-After` in both forms |
| Retry logic | `downloader-core/src/retry.rs` | classification, backoff curve and cap, `Retry-After`, separate budget for throttling |
| Rate limiting | `downloader-core/src/limiter.rs` | pacing, live changes, hierarchy (global > category > download), tightest bucket wins, cancellation |
| Queue priority | `downloader-core/src/scheduler.rs` | highest priority first then FIFO, re-prioritising a waiter, cancelled waiters never leak a slot |
| Scheduler (time windows) | `crates/scheduler` | same-day and overnight windows, day-of-week, exact boundaries |
| Path validation | `path-safety`, `download-manager/src/validate.rs` (+ robustness), `tests/integration/tests/fs_security.rs`, `hostile.rs` | see 13.4 |
| URL validation | `validate.rs` (`urls`, robustness), `batch.rs` | schemes, credentials, control characters, length |
| **State transitions** | `tests/integration/tests/state_machine.rs` | **all 36 combinations** of (queued, downloading, paused, failed, completed, cancelled) x (start, resume, pause, cancel, retry, delete): allowed ones work, refused ones return `invalid_state` and change nothing; then 6 rounds of 40 racing commands on one download always end in one consistent state and a byte-exact file |

### 13.2 Integration tests against controlled servers

`tests/integration/src/lib.rs` is a small HTTP server whose behaviour is chosen by the path (table in its module documentation).
`tests/integration/tests/servers.rs` (and the older `phase2`-`phase7`) run the real engine or manager against it:

| Server | Test |
|---|---|
| normal / range | parallel, byte-exact (`a_range_server_is_downloaded_in_parallel_and_exact`, `phase2`, `phase3`) |
| no-range | `/norange/`: one connection, exact |
| slow | `chunk_delay` (throughput tests), `head_delay` (slow to answer: waited for, and **Cancel is immediate**) |
| unstable | truncated bodies resume at the written offset; `/reset/` (closes with no answer); `/hang/` (headers then silence): read timeout fires, no partial file is published; `/lie/` (short body): never published |
| redirect | `/redir/N/`: chain followed to the limit; `/redirloop`, 40 hops: refused; `/redirext`: credentials **not** forwarded to another origin |
| ETag | changed validator forces a clean restart (`phase2`) |
| authentication | `/auth/`: 401 without credentials (not retried blindly), credentials from the link (never shown back), wrong password (never echoed) |
| 429 | per-connection limit makes the engine back off; `/e429/`: bounded retries then a clear failure |
| 500 | `/e500/`, `fail_next` (503): retried within the budget, then a clear failure |

### 13.3 Crash tests (real processes)

`tests/integration/tests/crash.rs`. A worker process (the same test binary, started with `VELOX_CRASH_ROLE`) runs a real `DownloadManager`;
it is killed, then a fresh manager starts on what is left. After every crash the tests check that the SQLite file is sound
(`SqliteStore::integrity_check`), that nothing is shown as running, that the resumed download ends **byte for byte identical**
(random data, so a wrongly trusted byte cannot hide), with no `.part` or stray file and exactly one record, and that what was already durable is not fetched again.

| Roadmap item | How it is killed |
|---|---|
| kill process during download | `disk.after_write@4` (early), `@60` (late), and 8 kills from outside (`Child::kill`: TerminateProcess / SIGKILL) at seeded random moments, restarting each time |
| kill during disk write | `disk.after_write`, right after a write returns |
| kill between data and counters | `checkpoint.after_fsync`: the data is durable, the counters are not |
| kill during SQLite transaction | `store.before_commit@1` (first ever, so nothing exists afterwards), `@6` (mid-download, rolled back), `store.after_commit` |
| kill during pause | `pause.before_persist` (comes back paused with its progress) |
| kill during resume | `resume.after_load`, then again in the resumed transfer: three crashes in a row |
| kill during finalization | `finalize.before_rename` (complete `.part`: **not downloaded again**), `finalize.after_rename` (file in place: recognised as finished, **the server is not contacted**) |
| one-connection download | killed mid-transfer: restarts cleanly |
| kill while the file is reserved | `disk.after_preallocate`: the record already exists, it simply resumes |
| kill while the history line is written | `history.before` / `history.after`: the file is complete, nothing is fetched again, no duplicate history |
| kill while deleting | `delete.after_files` / `delete.before`: the record can be deleted again and the folder ends clean |
| kill in the middle of a batch | `batch.after_item@4`: exactly the links added so far exist, none started; adding the list again recognises them (4 existing + 4 new) |
| kill while a setting is written | `settings.before` -> old value, `settings.after` -> new value, never anything else |
| damaged settings | half-written / garbage / overflowing values for every setting: the app starts with defaults (a broken schedule never silently holds downloads) |
| kill during a stream (HLS) download | `media.after_segment@9`, `media.before_mux`, `media.after_mux`, `media.after_link` and 5 random external kills: afterwards the folder holds nothing or the complete correct file, never an empty placeholder or a leftover working folder, and the same download works again |
| **power loss** (simulated) | after a kill the test damages what a power cut damages: the end of the SQLite log is cut (75 %, 40 %, 5 %, all), every `.part` byte the database does not claim becomes garbage, or the `.part` file comes back shorter. First it checks that **every byte the database claims equals the original**; then recovery must still end in a byte-exact file |
| **disk and database faults** (injected, `VELOX_FAILPOINT=name@N:enospc|denied|notfound|io`) | disk full mid-transfer and at reservation, the disk disappearing, an antivirus / lock making the final rename fail, every fsync failing, the state database refusing writes: the download fails within seconds with a clear reason (no hang), nothing wrong is ever published, and after the fault the retry finishes the file (for the locked rename without downloading anything again) |

How the failpoints work: `failpoint::hit("name")` is an empty function in every shipped build. Only `tests/integration` (a dev-dependency)
switches on the `failpoint/enabled` feature, and then `VELOX_FAILPOINT=name[@N]` makes the N-th hit end the process with `SIGKILL` (Windows: abort), no unwinding, no flushing.
`policy.test.mjs` proves that no shipped program enables the feature and that every failpoint in the code is used by a crash test.
The suite was checked by mutation: making the checkpoint over-claim progress by 300 KB makes 7 of the 13 tests fail.

Limits: the kill tests are *process* crashes, where the operating system keeps what it had accepted. The power-loss rows model what a real power cut adds (unflushed file bytes, the end of
the database log, a shorter file) by damaging the files after the kill; they cannot reproduce every reordering a real disk or file system may do, and nothing here corrupts the disk itself. The
promise they check is the one the code makes: data is fsynced before the counters that describe it are committed (`engine::checkpoint`), and SQLite's WAL only ever loses whole trailing commits.
The tests were run on Windows; the same tests run in CI on Linux and macOS (the Unix kill path is `SIGKILL`) but were not run there by the author.

### 13.4 Security tests

| Roadmap item | Where |
|---|---|
| path traversal | `hostile.rs` (37 server-chosen names, `..`, both separators, drive letters, UNC, encoded), `fs_security.rs`, `path-safety` property test |
| malformed URLs | `validate.rs` robustness test, `batch.rs`, `hostile.rs` (2 MiB link) |
| malformed headers | `hostile.rs` (broken `Content-Disposition`: unterminated quote, bad percent-encoding, unknown charset, empty), `http-client` robustness |
| oversized payloads | `/big-header/` (8 KiB fine, 4 MiB refused), 2 MiB link, `/huge/` (**a server claiming a petabyte is refused before any space is reserved**), bridge frame limits (`browser-bridge`), batch limits |
| invalid JSON | `hostile.rs` (26 malformed / hostile documents against every IPC structure: unknown fields, wrong types, `__proto__`, 5000-deep nesting, trailing garbage), `phase4` |
| IPC abuse | `tests/e2e/desktop-ipc.mjs` (hostile input against the real app), `authz.rs`, capability policy tests |
| extension spoofing | `browser-bridge` `caller` tests (id allow-list), `link.rs` / `host.rs` (impostor server, wrong caller), `real-browser.mjs` |
| localhost abuse | no TCP/UDP listener (policy), `redirect_decision` tests, the public-to-local redirect refusals |
| symlink attacks | `fs_security.rs`, `path-safety` (no-follow opens, hard links) |
| Unicode paths | `hostile.rs` (accents, CJK, emoji, right-to-left in folders and names; bidi override and zero-width characters removed from server names) |
| special filenames | `hostile.rs`: `CON`, `NUL`, `com1`, `LPT9`, `name.`, `a.txt::$DATA`, `..`, control characters, 400-character names |
| malicious archives | `hostile.rs`: a ZIP and a TAR whose entries are `../../evil.txt` are saved as plain files and nothing is extracted; policy test: no archive library is linked |
| FFmpeg argument injection | `ffmpeg.rs` (`no_file_name_can_inject_an_ffmpeg_argument`: dozens of hostile names always give the same argument list), `media.rs` (the real process receives the arguments; no shell) |

## Defects these tests found (and that are fixed)

* **Two downloads with the same file name shared one partial file, and the last one replaced the other's finished file**
  (found while adding "download again"): the engine only checked that the *final* file did not exist when it started, the
  partial file was created with truncation, and the final rename replaces. Now the name is reserved when the download
  starts (`name (1)`, `name (2)`... across files on disk, running downloads and unfinished ones in the database), and a file
  that appears under the name while downloading is never replaced. Tests: `usability.rs` (same name at once, a file appearing
  mid-download, a paused download keeps its name), `downloader-core/tests/download.rs`.

* **A full or failing disk hung the download** (found by the fault-injection tests): when the writer of a segment stopped on a disk error, the reader kept waiting on a full channel for a writer that no longer
  existed, so the download stayed "downloading" forever. The writer now hands the channel back when it stops; the download fails at once with the disk's message and the retry resumes.
* **A killed stream download left an empty placeholder file** (the finished name was reserved with an empty file before the real one was renamed over it) **and a hidden working folder for ever**. The file is now put in
  place with a hard link (no placeholder; crash-safe), and working folders of dead processes are removed at the next start.

* **Pause / Cancel waited for a slow server** (Phase 12 for streams, Phase 13 for ordinary downloads): while the engine waited for the answer to its first request, Pause and Cancel were not observed (up to the 30 s request timeout, per retry). Fixed in `probe_with_retry`.
* **No free-space check**: a server (or a mistake) announcing a size larger than the disk was passed straight to the pre-allocation. It is now refused first, with the reason shown ("not enough free disk space").
* **`Retry-After` as an HTTP date was ignored** (only the number of seconds was understood). Both forms are parsed now.
* **A store selector that built a new array looped React** (found by `desktop-smart-queue.mjs` while building the Smart Download Queue,
  before it shipped): `useStore((s) => s.queues?.queues ?? [])` answered a new `[]` on every read while the queues were not loaded,
  so the window showed "Something went wrong". Fixed in the Add dialog; `src/selectors.test.ts` now rejects any selector that builds
  a new array or object.
* **Moving a download up did not move it on screen** (found by the same e2e test): only the moved download was announced, not the
  one it traded places with. Both are announced now (`smart_queue.rs` checks it).
* **The e2e window-close checks failed where a tray exists** (since the tray came in, closing the window keeps the app running in
  it): the harness now turns "close to tray" off before a test closes the window to quit (`quitOnClose` in `harness.mjs`, and
  `linux-webdriver.mjs`).

## Download folders and sorting

`tests/integration/tests/storage.rs` (12 tests) and `tests/e2e/desktop-folders.mjs` (real app, 39 checks). Unit tests: `path-safety/src/folders.rs` (folder validation, allowed set),
`downloader-core/src/filetype.rs` (extension / content type, hostile names). They cover: the old behaviour unchanged when sorting is off; every type goes to its folder; renamed folders and their strict checks; changing the
default folder (old downloads still resumable and deletable, restart, reset); a folder for one download (Add dialog, confirmation popup); made-up tokens and forbidden folders (system, program, autostart, `.ssh`, drive
root, the app's own data); an unavailable chosen folder; a choice half-saved before a kill; streams into `Video`. The e2e test answers the native dialog through a file (debug builds only, `VELOX_TEST_PICK_FILE`).

## Everyday features (before Phase 14)

`tests/integration/tests/usability.rs` (13 tests): the Add dialog's sign-in and headers reach the server and are sealed on
disk; a `401` becomes "sign in and retry"; a real proxy round trip (the test server plays an HTTP proxy and checks
`Proxy-Authorization`), network settings kept / cleared / refused and restored after a restart with the password sealed;
a speed limit for videos slows only videos; show in folder / open in a dry run (programs refused, a record edited to point
outside the download folders is refused); history removal keeps the lifetime total; errors in plain words; same-name downloads.
`tests/e2e/desktop-usability.mjs` (48 checks) drives all of it in the real app, plus paste / drop, shortcuts, the Settings side
menu and the confirmation popup following a rename. Every e2e run sets `VELOX_DRY_RUN_SHELL=1` (debug builds only), so
"Show in folder" / "Open" never open windows during tests.

## Observability (Phase 14)

`crates/diagnostics` unit tests: the redactor (headers, pairs, JSON, tuples, `Bearer` / `Basic`, links with credentials, query
values and fragments, the home folder, ordinary text left alone, idempotence, garbage and huge input), the logger (levels and the
detailed switch, library targets, span context, rotation, the memory ring, a folder that cannot be used, private files on Unix,
crashes written without their secrets). `tests/integration/tests/diagnostics.rs` (5 tests): a download with retried `503`s reports
its attempts, retries, `206`, connections, speed and timeline and logs one line per run; a `404` keeps its code and status across
a restart and goes away with the download; pause / resume counts attempts; detailed logging is remembered, the log folder is
shown in a dry run; and downloads carrying a password, cookies (typed and from a browser), a signed link with credentials, a
fragment and a proxy password leave none of them in the log ring, the log file, the diagnostics, the reports or (for the real
secrets) the database. `tests/e2e/desktop-diagnostics.mjs` (28 checks) does the same through the real app.

## Videos from the browser and 64 connections

`apps/browser-extension/test/media.test.mjs`: what counts as a video (types, sizes, pieces, failures, YouTube's own pieces), one
stream per host, bounded lists, video pages and DRM services, the bar's and the popup's messages. `package.test.mjs`: `webRequest`
read-only (never blocking), the bar registered only at run time and in top frames, closed shadow root, no link in its messages, the three site
lists (extension, app, UI) identical. `media_engine::site` unit tests: yt-dlp arguments (fixed, link last after `--`, no cookies),
quality ids and file types closed sets (each type gets its own fixed options: MP4 never ends as .mkv), qualities from yt-dlp's JSON, DRM / live / playlists refused, progress and error lines.
`tests/integration/tests/video.rs` (11 tests, with the stand-in `fake-ytdlp`): a page analysed and downloaded, every file type (MP4 by default), pause / resume / watch while downloading / dashboard counts, a failure tried again by itself and saved in the history, DRM never retried, without ffmpeg,
a browser video waiting for the dialog with its headers (never its cookies), dismiss, DRM refusals, no yt-dlp, cancel.
`tests/e2e/desktop-video.mjs`: the same through the real native host and app, plus 64 connections by default.
`tests/e2e/desktop-redesign.mjs` (52 checks): the frameless title bar and window buttons, Mica on Windows 11, the sidebar,
the 8x8 connection matrix, six window sizes from 1920 x 1080 down to 640 x 480 (nothing scrolls sideways, the sidebar folds
under 900 px, the Smart throttle is shown once, cards stay inside the page; `responsive-*.png`), the Smart throttle HUD turning the accent amber (and the real setting), French, Arabic
(right-to-left mirroring, Cairo font, progress filling from the right). `tests/e2e/shot-window.mjs` takes real screen
captures of the window (Mica can't be seen through the debug port).
`tests/e2e/desktop-views.mjs` (38 checks): Downloads, Active and Completed are three different views (dashboard; work in progress
with live speed, connections in use, time left and still to download, Pause all / Resume all; finished files with search, kinds, open,
show in folder, copy link, remove from list, delete from disk); deleting a file from the disk always asks first (Cancel is focused and
deletes nothing, "Remove from list only" keeps the file), from the Completed page and from a card's menu; both pages at four window
sizes without sideways scrolling (`views-*.png`).
`tests/e2e/real-video-limits.mjs` (by hand, internet, real yt-dlp / Deno / ffmpeg / ffprobe:
`node tests/e2e/real-video-limits.mjs target/debug/velox-desktop.exe <tools folder>`): MP3 keeps moving while ffmpeg converts and is
real sound; MP4 only moves forward and has picture and sound; the global limit and the Smart throttle slow a running video and lifting
them brings the speed back, with one yt-dlp at a time and none left after Cancel.
`tests/e2e/real-browser.mjs`: in a real Brave, detection, the badge, the bar over a playing video, a click made up by the page ignored and a
real click handing the video over. Planner tests pin the new connection table (1-64).

## Smart throttle and Live preview

See the Tests section of `docs/smart-throttle-and-preview.md`. Two engine regressions found by these tests are pinned:
`disk::tests::a_range_delivered_in_one_chunk_still_pays_the_speed_limit` and
`limiter::tests::lifting_a_limit_wakes_connections_that_are_waiting_under_it`.

## Adding a test

* A new server behaviour: add a path prefix to `tests/integration/src/lib.rs` and document it in the table at the top.
* A new crash point: `failpoint::hit("area.what")` in the code, and a scenario in `crash.rs` that kills there (the policy test fails until there is one).
* A new command or state: extend the table in `state_machine.rs` (the compiler forces every state to be listed).

## End to end

`tests/e2e/*.mjs` start the real debug app and drive its window. Build first:
`cargo build -p velox-desktop -p velox-cli -p velox-integration-tests --features velox-desktop/custom-protocol`, `npm --prefix apps/desktop run build`,
then `touch apps/desktop/src-tauri/src/lib.rs`, and run e.g. `node tests/e2e/desktop-queue.mjs`. `real-browser.mjs` needs a Chromium browser and must be run
without administrator rights (see `docs/manual-test.md`).

**Linux.** The same scripts run unchanged: `harness.mjs` drives WebKitGTK over WebDriver (`linux-webdriver.mjs`) instead of
WebView2's DevTools port. Needs `WebKitWebDriver` (package `webkit2gtk-driver`) and `Xvfb` (package `xvfb`): each run gets its own
virtual display, and KWin (when installed) as window manager in a private D-Bus session, so nothing appears on, or changes, the
user's desktop. `VELOX_E2E_DISPLAY=:0` runs on the real display instead; `VELOX_E2E_WM=0` skips the window manager. Windows are
closed like a user would (`x11-close-window.py`, WM_DELETE_WINDOW). Not ported yet: `desktop-browser.mjs` (Windows registry checks)
and the `real-*` scripts. Known Linux gaps they show: live preview does not play (WebKitGTK cannot stream media from a custom
protocol). (`desktop-ui.mjs` used to measure ~20 fps while downloading; since the paint fixes of `docs/ui-performance.md` it holds
60 fps and passes.) On a machine without root, both tools can be unpacked from their packages into
a user folder (`dpkg -x`) and put on `PATH`. Still failing on Linux, before and after the Smart Download Queue (checked on the previous
commit too): `desktop-queue.mjs` waits in vain for the second dry-run power action at its very end.

## Fixes from testing on other computers

* `velox-native-host.exe` must not import `VCRUNTIME140.dll`; the security gate checks the release binaries' imports.
* First launch: measured by launching the debug app (with `custom-protocol`) with fresh `WEBVIEW2_USER_DATA_FOLDER`,
  `VELOX_DATA_DIR` and `VELOX_BRIDGE_ENDPOINT` folders and capturing the window from the screen each second. The WebView2 crash
  (an injected overlay) was confirmed from the minidump in `EBWebView/Crashpad/reports`; recovery was tested by killing the WebView2
  browser process (`msedgewebview2.exe` without `--type=`).
* e2e: every app the harness starts gets its own `VELOX_BRIDGE_ENDPOINT` (the app is single-instance by that link).
* `phase5.rs` `disk_space_is_known_before_the_download_folder_exists`; `link.rs`
  `a_second_instance_cannot_take_the_link_and_is_told_why`; `webview_guard` budget test.

## Video tools

`tests/integration/tests/tools.rs` serves stand-ins for the download servers (verified install, an interrupted download continued, parts of another file thrown away, installer copies used, wrong checksum refused, ffmpeg
fallback source, 8 parallel parts). Opt-in, against the real servers:
`VELOX_TEST_REAL_TOOLS=1 cargo test -p velox-integration-tests --test tools real -- --nocapture` and
`VELOX_TEST_REAL_YOUTUBE=<watch link> cargo test -p velox-integration-tests --test tools youtube -- --nocapture`.
Debug builds of the desktop app fetch tools only with `VELOX_TOOLS=official`.

## Desktop behaviour and Smart throttle automatic mode

* `tests/integration/tests/desktop.rs` (3 tests): desktop defaults (close to tray, notifications on, start at login off),
  start at login set up / removed / kept pointing at the app (moved AppImage), choices persist, bad language codes refused;
  all through the dry-run system.
* `apps/desktop/src-tauri/src/desktop.rs` unit tests: every language has the tray / notification texts, only a download
  that was running and just ended is announced, the second-launch socket (and its short-path fallback).
* `crates/browser-bridge`: `launch.rs` (the app found for every install layout and from the recorded path; started on its
  own), `tests/host.rs` (a download starts the app, a ping never does), `tests/link.rs` (the link opens on the very first
  start before the data folder exists).
* `tests/integration/tests/smart_preview.rs` + `manager/smart_throttle.rs`: see `docs/smart-throttle-and-preview.md`.
* By hand (`--ignored`): `system.rs` real notification + logind inhibitor, real idle time; `launch.rs` real desktop portal.

## Smart Download Queue

Described in `docs/smart-download-queue.md` (section 7 lists every suite):

| Where | What |
|---|---|
| `crates/scheduler/src/queue_rules.rs` | 11 unit tests of the rules |
| `crates/downloader-core/src/scheduler.rs` | 5 for order, held-back waiters and the express lane |
| `crates/persistence` | 1 for places |
| `crates/system-integration/src/network.rs` | 3 for the routing table and adapters |
| `manager/queues.rs` | 3 unit tests |
| `tests/integration/tests/smart_queue.rs` | 14 tests through the manager, with a fake clock and the dry-run system's idle time and connection (`DryRunSystem::set_network`) |
| `apps/desktop/src/smartQueue.test.ts` | 12 UI logic tests |
| `apps/desktop/src/selectors.test.ts` | 1 |
| `tests/security/policy.test.mjs` | 1 ("Smart Download Queue") |
| `tests/e2e/desktop-smart-queue.mjs` | 47 checks in the real app; its screenshots are `docs/screenshots/smart-queue-{light,dark}.png` |

`desktop-ui.mjs` now picks *High* in a queue row (the queue page offers High / Normal / Low).

## Download Doctor

Described in `docs/download-doctor.md` (section 8):

| Where | What |
|---|---|
| `crates/download-manager/src/doctor.rs` | 10 unit tests of the rules |
| `crates/download-manager/src/explain.rs` | 6 more asserts for the new error codes |
| `tests/integration/tests/doctor.rs` | 7 tests with real failures (500, 429, 401, a web page, reset, hang, a refused port, `.invalid` DNS, offline through `DryRunSystem::set_network`, TLS to a plain server, a disk full, a checksum mismatch twice, a recovery through Retry). It uses `serve_on(addr, data)`, a new helper in `tests/integration/src/lib.rs` |
| `apps/desktop/src/doctor.test.ts` | 4 UI tests, including every key in en / fr / ar |
| `tests/security/policy.test.mjs` | 1 ("Download Doctor"), 42 in total |
| `tests/e2e/desktop-diagnostics.mjs` | 4 more checks (the Doctor panel on a 404, and the timeline line); screenshot `docs/screenshots/doctor.png` |

**Defect found:** certificate failures were retried 5 times (6 connections for one check). It is fixed in
`http-client` (`tls_will_fail_again`) and guarded by the TLS integration test, which fails on the old code with
"6 connections".

## Download Intelligence

Described in `docs/download-intelligence.md` (section 7):

| Where | What |
|---|---|
| `crates/download-manager/src/intelligence.rs` | 17 unit tests of the rules (no network) |
| `tests/integration/tests/intelligence.rs` | 10 tests against the local test server (`/norange`, `/redir`, `/auth`, `/html`, `/e500`, `/huge`, `head_delay` for cancellation) |
| `apps/desktop/src/intelligence.test.ts` | 5 UI line tests, including every key in en / fr / ar |
| `tests/security/policy.test.mjs` | 1 ("Download Intelligence"), 41 in total |
| `tests/e2e/desktop-intelligence.mjs` | 25 checks in the real app; screenshots `docs/screenshots/intelligence-{light,dark}.png` (and `add-advanced-dark.png` now shows the Check link button) |

Building on this Linux machine: `. ~/.local/sysroot/env.sh` first (the dbus / WebKit development files are unpacked
there). `desktop-ipc.mjs` also needs the command-line binary (`cargo build --bin velox`).


## Smart throttle and download speed: steadiness fixes

Found by measuring on a real line (a ~3 MB/s connection, public test servers, a competing download started by hand);
every fix has a test that fails without it. Details in `docs/smart-throttle-and-preview.md` ("What was wrong") and
`docs/architecture.md` (performance model).

| Where | What |
|---|---|
| `crates/downloader-core/src/adaptive.rs` | a dip never cuts a few connections; a throttling server is probed less and less often; useless probes back off; a 429 halves once (4 new tests) |
| `crates/downloader-core/src/limiter.rs` | a shared limit shows as waits; lowering a limit never stops downloads (no 0-byte gap over 700 ms; it was ~2 s); raising one speeds up without a stop (it was ~0.9 s); `any_limit` (3 new tests) |
| `crates/downloader-core/src/disk.rs` | a slow connection reports progress at least every 0.5 s (it was every 1.8 s) |
| `crates/download-manager/src/manager/smart_throttle.rs` | the measurement (same window, headers, speed changes, gaps), the cap is a share of today's speed, settings move the cap without lifting it, buffering swings cancel out, readings recorded on a real capped line are not browsing, noise at high speed (7 new tests) |
| `crates/download-manager/src/manager/media.rs` | only a real change of limit restarts a video job |
| `tests/integration/tests/smart_preview.rs` | modes flipped while capped: bytes every half second through the real engine (it stood still for 1.1 to 1.4 s after each change) |
| `apps/desktop/src/throttle.test.ts` | the shared state: shown at once, a stale poll never brings the old state back, quick clicks in order and the last wins, off shows full speed at once, a refused change brings the real state back, the gauge (5 tests) |
| `tests/e2e/desktop-throttle.mjs` | 23 checks in the real app (Linux and Windows): clicks, modes, the app's own process as "a game", a capped download that never stops, the gauge's core in place, mode names that fit, Settings and the card agree; screenshots `docs/screenshots/throttle-hud-limiting-dark.png`, `throttle-settings-dark.png` |
| `tests/e2e/harness.mjs` | `startFileServer(...).rateBps`: one speed shared by every connection, like a real line |
| `tests/security/policy.test.mjs` | the watcher's "full speed on any failure" in its new shape; the traffic is read from totals only (`sysio.rs`) |

On Linux `desktop-preview.mjs` stops before its Smart throttle part (live preview does not play in WebKitGTK, see
above); `desktop-throttle.mjs` covers that part.

## Interface smoothness and start-up

Details in `docs/ui-performance.md`.

| Where | What |
|---|---|
| `apps/desktop/src/perf.test.ts` | 8 tests: the backdrop picture has the CSS lights (same order, own-colour fade, scaled ellipses) and is used; the window is asked for at commit by a direct call; the Linux block turns shadows off, keeps meaningful rings as outlines, drops drop-shadow filters, keeps the glass highlight |
| `tests/e2e/desktop-perf.mjs` | 5 checks in the real app, prints the numbers: shown within 150 ms of the script, painted within 1 s, >= 45 fps with downloads running, >= 40 fps scrolling over them, no shadow on Linux cards / panels. `VELOX_E2E_DISPLAY=:0` (real screen), `PERF_WAYLAND=1`, `PERF_CSS` (a stylesheet laid over the page, to find what costs) |
