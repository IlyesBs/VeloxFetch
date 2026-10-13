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

* **Smart Duplicate Detection** (`docs/smart-duplicate-detection.md`). Before, the Add bar only compared the exact text of
  a link with the list, said the same thing whatever the state, and a same-link match was claimed even when the server's
  file had changed. Tests:
  * `download-manager` `intelligence::tests` (6 new): normalisation, version rules, checksums, order, file state.
  * `tests/integration/tests/duplicates.rs` (9): states, file gone or edited, history and restart, checksum, name
    conflict vs. duplicate, identical requests at once, secrets, malformed links.
  * `apps/desktop/src/duplicates.test.ts` (9) and `intelligence.test.ts`: prompt per state, name-conflict lines, keys
    in en/fr/ar.
  * e2e `desktop-usability.mjs`: the real prompt for a finished file offers Open, Show in folder, Download again.
  * Results (Linux, 2026-10-10):
    * Rust workspace: 702 passed, 0 failed (`duplicates.rs` 3 runs in a row, all green);
    * vitest: 110;
    * policy: 42/42;
    * clippy and fmt: clean;
    * e2e: 17 of 18 suites passed then. `desktop-preview.mjs` failed on the base commit too. It was first thought to
      be the machine, but it was an app bug (a 503 for bytes not written yet, and the file's start not fetched first),
      fixed right after: see "Preview on Linux and macOS".

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
and the `real-*` scripts. (Live preview on Linux: fixed, see "Preview on Linux and macOS" below.) (`desktop-ui.mjs` used to measure ~20 fps while downloading; since the paint fixes of `docs/ui-performance.md` it holds
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

On Linux `desktop-preview.mjs` runs in full since the private preview address (October 2026); see "Fixes: all known
issues" below.

## Interface smoothness and start-up

Details in `docs/ui-performance.md`.

| Where | What |
|---|---|
| `apps/desktop/src/perf.test.ts` | 8 tests: the backdrop picture has the CSS lights (same order, own-colour fade, scaled ellipses) and is used; the window is asked for at commit by a direct call; the Linux block turns shadows off, keeps meaningful rings as outlines, drops drop-shadow filters, keeps the glass highlight |
| `tests/e2e/desktop-perf.mjs` | 5 checks in the real app, prints the numbers: shown within 150 ms of the script, painted within 1 s, >= 45 fps with downloads running, >= 40 fps scrolling over them, no shadow on Linux cards / panels. `VELOX_E2E_DISPLAY=:0` (real screen), `PERF_WAYLAND=1`, `PERF_CSS` (a stylesheet laid over the page, to find what costs) |

## Gaming policy (Smart throttle)

Details in `docs/smart-throttle-and-preview.md` > Gaming policy.

| Where | What |
|---|---|
| `manager/smart_throttle.rs` | 6 tests: `GameWatch` on at the first sighting / off after two scans, a flickering game never flips it, several games, failed scans kept 15 s then lifted (given clock, no waiting), each policy's cap is never zero, launchers / helpers / look-alikes never match |
| `src-tauri/src/smart_throttle.rs` | a program that renamed its main thread is found by its executable's name (a copy of the test binary; fails without the executable match) |
| `tests/integration/tests/smart_preview.rs` | 4 new tests through the real manager and engine: pause (only what it paused resumes, intact; the user's pauses before / during the game stay; nothing new starts; a second game keeps it), the user's queue pause and a limit changed during the game outlast it, a restart during a game leaves nothing held, low / switching policy mid-game / flicker / off / bad limits; the first test now lifts after two scans and keeps the policy through a failed scan |
| `tests/integration/tests/smart_queue.rs` | the call ends after two scans |
| `apps/desktop/src/throttle.test.ts` | the status line for low / pause, never "a game" without one; the policy is saved like other settings |
| `tests/e2e/desktop-gaming.mjs` | 17 checks in the real app with a stand-in game (this runtime copied under a test name and left idle): the Settings choices, pause / resume through the real process scan, the card and Queue view texts, low speed held, off |
| `tests/security/policy.test.mjs` | "smart throttle": executable file names only, bounded grace, game hold never saved, gate rule, bounded limit |

Results (2026-10-09, Linux): Rust 669 passed (workspace); frontend 91; policy 42/42; e2e gaming 17/17, throttle, ipc,
views, redesign pass; `desktop-queue.mjs` stops at its last dry-run power step as before this change. Once, under the
full parallel run, `smart_preview.rs`'s preview-order check (a timing bound, no gaming code) failed and passed 3/3
when rerun.

## Fixes: all known issues (October 2026)

| Issue | Cause | Fix | Test |
|---|---|---|---|
| `desktop-queue.mjs` failed at its last step on every run | the dry-run power line had become `[dry-run] Power(Sleep)` (Phase 15), the test reads `[dry-run] power action Sleep` | the readable line is printed again (`dry_run_line`) | `system.rs` `a_dry_run_power_action_prints_the_line_the_end_to_end_tests_read`; the suite passes |
| Preview order could lose the end of a video under load | segments queued for a connection slot in the order their tasks happened to run, not the planned order | `AdaptiveGate::acquire_in_turn`: first connections queue in plan order | `adaptive.rs` `first_connections_get_their_slot_in_plan_order_whatever_order_the_tasks_run` (fails without) |
| Live preview did not play on Linux | WebKitGTK's player refuses custom schemes | private 127.0.0.1 address (12.2), streamed like a file being written | `preview_server` tests, `preview.test.ts`, `desktop-preview.mjs`, `desktop-video.mjs` |
| Gaming pause only slowed video jobs | they are not in the download queue | paused / resumed through their own controls | `media.rs` `the_pause_gaming_policy_pauses_running_videos_and_resumes_them_after_the_game` (fails without) |
| No full executable paths in the program list | names only | absolute paths accepted and matched exactly | `smart_throttle.rs`, `src-tauri` path test, `smart_preview.rs` |
| A limit just set showed only after ~5 s (`desktop-ui.mjs` failed) | the 2 s speed smoothing | the speed shown never exceeds the global / Smart / own limit (`shown_speed`) | `manager.rs` `a_limit_shows_at_once_and_the_rest_stays_smooth`; the suite passes 3/3 |
| Settings side menu highlight ran through the sections (flaky check) | smooth scroll fired the scroll-spy on the way | the clicked section stays until the scroll stops or the user scrolls (`spyLock.ts`) | `spyLock.test.ts`; `desktop-usability.mjs` |
| A running download could be announced to the window as "queued" | the run was announced before the engine stored "downloading" | announced at a later tick | `desktop-smart-queue.mjs` |
| `desktop-smart-queue.mjs` failed now and then | its click helper failed silently and clicked before the window switched buttons; its menu helper looked in every menu | wait for the element, fail loudly; pick the item in the menu just opened | 10 runs in a row |
| `desktop-browser.mjs` could not run on Linux | Windows-only pipe, registry and attach | temp-folder socket, scratch home for manifests (`VELOX_NMH_SCRATCH_HOME`, debug only), cold start checked in the database, then relaunched | passes on Linux |

Results (2026-10-09, Linux): Rust 676 passed; frontend 98; policy 42/42; e2e all pass: preview, video, media, gaming,
queue, throttle, ipc, views, redesign, usability, perf, ui, smart-queue, intelligence, diagnostics, batch, browser,
folders. `desktop-preview.mjs` skips "it plays while downloading" when this computer's sound server takes no stream
(it checks with `paplay`; on the development machine the PulseAudio layer of PipeWire hangs every client, `paplay`
included); everything else in it, bytes arriving while downloading included, is checked.

## Steady speed and a smooth Smart throttle (October 2026)

Found by measuring on a real line (3 MB/s; Hetzner, which refuses above ~2 connections, and thinkbroadband, which
allows ~32 and answers `Retry-After: 30`) with temporary probes printing every tick (deleted before the commit).
Details: `docs/smart-throttle-and-preview.md` > "Steady speed…", `docs/architecture.md` > Performance model.

| Where | Test | Fails without |
|---|---|---|
| `adaptive.rs` | `a_refusal_holds_new_requests_back_and_the_wait_grows_while_refused` (the wait, its growth, a burst counts once, `Retry-After`) | the hold |
| `adaptive.rs` | `after_a_refusal_only_the_limit_goes_when_the_hold_ends_not_a_storm` | the hold (20 requests at once) |
| `adaptive.rs` | `a_single_refusal_after_a_long_steady_run_keeps_the_connections` | the 20 s rule (2 → 1) |
| `adaptive.rs` | `a_lower_limit_makes_exactly_the_extra_connections_give_way` | `give_way` |
| `limiter.rs` | `a_connection_that_leaves_while_waiting_hands_its_share_back` | the refund (4 s instead of 1) |
| `engine.rs` | `a_limit_keeps_only_the_connections_it_needs` | `capped_connections` |
| `smart_throttle.rs` | `what_the_system_still_takes_in_right_after_slowing_down_is_not_someone_else`, `a_window_a_few_percent_off_at_full_speed_is_not_browsing`; the browsing tests confirm on 3 readings | settle / the 10 % allowance |
| `tests/integration/tests/phase3.rs` | `a_connection_limited_server_gets_runs_of_parts_not_a_request_per_part` (≤ ⅓ of the parts in requests, ≤ 6 refusals) | runs, the hold |
| `phase3.rs` | `runs_still_spread_the_last_parts_over_every_connection` (8 connections used, < 2.5 s instead of ~6) | shrinking runs |
| `phase3.rs` | `a_cap_keeps_few_connections_holds_its_speed_and_never_stops_the_download` (≤ 4 connections, no 0.5 s without bytes, within 12 % of the cap, all back on lift) | trimming, pieces, settling the debt (it overshot by 15–50 % or stopped) |
| `phase3.rs` | `a_download_started_under_a_cap_keeps_few_connections_and_its_speed` (≤ 3 connections, within 15 % of the cap) | starting on 2 when the free speed is unknown |
| `smart_preview.rs` | `changing_modes_while_capped_never_stops_a_running_download` now passes every time (it stopped 0.6–1.3 s when the cap went 4.5 → 0.9 MB/s) | settling the debt on a lower limit |
| `phase2.rs` | `mid_body_disconnects_resume_from_the_written_offset`: a connection lost inside a run counts as a retry of that part | |
| `apps/desktop/src/throttle.test.ts` | `polls that bring the same state never touch <html> again`, `slowing downloads or not keeps the same colours (no repaint)` | the flash fix |

## Preview on Linux and macOS (October 2026)

`desktop-preview.mjs` failed on Linux before this fix, on the base commit too. GStreamer said
`R2: Received 503 HTTP error code`, and once the start of the file was not ready after 20 s. Full analysis:
`docs/smart-throttle-and-preview.md`, "Linux and macOS: why the preview stopped". Tests added:

* `smart_preview.rs::watching_a_download_already_running_gets_its_start_first`. A real download at 200 KB/s, with
  "Watch" pressed while it runs:
  * the start is playable within 8 s (about 2 s now; it never was before);
  * the first unfinished part never sits idle for 4 s while others download;
  * 15 runs in a row green.
* `adaptive.rs`:
  * `a_part_queuing_again_keeps_its_place`: part 0 queuing again goes before parts 1–3;
  * `a_cancelled_wait_leaves_the_line`: a paused part does not block the line.
* `policy.test.mjs`: the private address on Linux and macOS only (Windows keeps the protocol); `Info.plist` allows
  local networking only.
* `desktop-preview.mjs`: macOS now expects the private address, and checks the secret, the Host and that scripts
  cannot read it, as on Linux.

Results (Linux):
* Rust: 705 passed;
* vitest: 110;
* policy: 42/42;
* clippy: clean;
* e2e: all 18 suites pass (`desktop-preview.mjs` 3 runs in a row).

macOS and Windows run in CI. A macOS type-check could not run here (it needs Apple's SDK). The macOS code path is
the Linux code under a wider `cfg`, with no new platform calls.

## Download Profiles (October 2026)

See `docs/download-profiles.md` §9. Tests:

* `profiles::tests` (7);
* `adaptive::tests::the_ceiling_follows_the_settings_while_running`;
* `tests/integration/tests/profiles.rs` (6; 3 runs in a row green);
* `apps/desktop/src/profiles.test.ts` (5);
* the profile checks in `desktop-throttle.mjs`.

Results (Linux):
* Rust: 719 passed;
* vitest: 115;
* policy: 42/42;
* clippy and fmt: clean;
* e2e: all 18 suites pass. Once, `desktop-throttle.mjs` aborted on a WebDriver call ("Session terminated without a
  reply") as the first suite right after a build. It was not reproduced in 4 runs in a row, all 32 checks passing,
  and no crash dump was found.

## Release script: AppImage media tools (October 2026)

`build-release.sh` failed at the AppImage on a machine that already had the Tauri libraries
(`failed to run linuxdeploy`; linuxdeploy's GStreamer plugin said `Error: patchelf not found`). Fixed in the script
(see `docs/release-process.md`). Checked:
* `bash -n` passes;
* the detection, run on this machine, finds the plugins
  (`/usr/lib/x86_64-linux-gnu/gstreamer-1.0`) and the helper, and reports `patchelf` missing when it is not on the
  PATH;
* `patchelf` installed in user space from pip.

Not yet checked: a complete AppImage built by the script (the release build runs only when asked).


## Resume integrity (October 2026)

See `docs/resume-integrity.md` §8. Tests:
* `crates/downloader-core/src/verify.rs` (6);
* `crates/http-client` ranged-answer checks (2 new);
* `tests/integration/tests/reverify.rs` (11): 3 runs in a row green. Added: a same-size change with no `ETag`,
  and one with an unchanged date, are caught at the joins mid-run and the retry gives entirely the new version; a
  change while paused with the same date is caught and never mixed;
* `tests/integration/tests/crash.rs`: killed while invalidating a changed segment (1 new; whole suite 33 green). The
  join checks cost 1 KiB per request: recovery still stays under the suite's "file + 64 KiB" budget;
* `engine.rs` `event_tests`: a flood of retries never pushes out the run's milestones;
* stress: 30 parallel copies of `reverify` (16 threads each) and 25 runs in a row, all green after the fixes below.
  Under load they first showed two real problems, both fixed: milestones evicted by retry lines, and diagnostics
  read in the moment between a run's end and the saving of its timeline;
* `relink.rs` and `reverify.rs` delete their folders when they finish. Leftover test folders in a RAM-backed `/tmp`
  (11 GB after many runs of the older suites) slowed the whole machine.

Results (Linux, 2026-10-11):
* Rust workspace 766 passed (after the join checks, timeline fixes and test-folder sweep); clippy (`--locked`,
  `-D warnings`) and fmt clean; vitest 130; tsc clean;
* `tests/integration/src/lib.rs` `sweep_stale_test_folders`: once per test process (Linux), removes `velox-*-<pid>-*`
  temp folders of test processes that have ended, so the temp folder no longer fills up (36 MB after a full run;
  it was 11 GB);
* policy 42/42. The usual commands (`cargo test --workspace`, `cargo clippy --all-targets`, the gate) leave a clean
  `target/debug/velox-desktop`. Only an ad-hoc `cargo build --workspace --all-targets` builds it with the crash-test
  hook (feature unification), which the policy check then rightly refuses;
* e2e views, ipc, intelligence, usability, browser, ui ×4: all pass.
* `desktop-ui.mjs`: the profile drop-down check read the field before the app's `profiles` event had re-rendered
  it (a controlled field). It failed 2 of 4 runs. It now waits for the field (up to 5 s); 3 of 3 runs pass. This is
  a test race, not an app bug: the field shows the pick once the event arrives.

## Use a new link (October 2026)

See `docs/new-link-recovery.md` §8. Tests:

* `crates/download-manager/src/relink.rs` unit tests (9);
* `tests/integration/tests/relink.rs` (11, mocked server: `/signed/<token>/` links that expire, `/noetag/`,
  `../dated/`; added to `tests/integration/src/lib.rs`);
* `apps/desktop/src/newLink.test.ts` (4);
* e2e `desktop-views.mjs`: card menu → Use a new link on a paused download; the same link refused; a fresh link
  proven and used; completes byte for byte; unknown id refused.

Results (Linux, 2026-10-11):
* Rust workspace 744 passed (relink: 3 runs in a row green); clippy and fmt clean; vitest 130; policy 42/42;
* e2e views, ui (×3), intelligence, ipc, usability, browser: all pass. One `desktop-ui.mjs` run had a one-off timing
  miss on the profile drop-down pick (the value was read before React re-rendered); the next two runs passed.
* `desktop-usability.mjs`: its finished-file menu expectation was out of date since Download again (the menu is
  `Open | Show in folder | Download again | Details | …` by design). Expectation updated.
* Release gate: the Rust steps pass with the build environment loaded. `npm audit` reports a high advisory in
  `source-map-js` (GHSA-68fv-2mgg-jv7q), a build-time dependency of the UI bundler. It is on `main` too and is not
  part of this feature.

## Download again (October 2026)

See `docs/redownload.md` §5. Tests:

* `tests/integration/tests/redownload.rs` (4; 3 runs in a row green);
* `apps/desktop/src/redownload.test.ts` (3);
* e2e `desktop-views.mjs`: a file deleted outside the app is shown missing after returning to the window; Download
  again is the main button and brings it back on the same entry; a second time gives `echo (1).pdf`; an unknown id
  is refused.

Results (Linux):
* Rust: 723 passed;
* vitest: 118;
* policy: 42/42;
* clippy and fmt: clean;
* e2e: `desktop-views`, `desktop-ipc` and `desktop-ui` all pass.

## Video download details (October 2026)

See `docs/video-details.md` §5. Tests:
* `clock_tests::the_details_clock_counts_running_time_only`;
* `apps/desktop/src/mediaDetail.test.ts` (4);
* the details-page checks in `desktop-media.mjs`. Two runs in a row green. A first run failed two checks that
  compared against "Time taken" / "Running for", while the labels are drawn in capitals (`.label` is uppercase,
  and `innerText` follows that). The checks now ignore case; the app was right.

Results (Linux):
* Rust: 724 passed;
* vitest: 122;
* policy: 42/42;
* clippy and fmt: clean;
* e2e: `desktop-media`, `desktop-views`, `desktop-ui` and `desktop-ipc` pass.

## Up to 64 connections everywhere (October 2026)

The Fast profile was set to 32 connections, and the planner limited files of 50–200 MB to 32 (10–50 MB to 16),
whatever the setting. Both now reach 64:
* Fast is 64;
* `planner::max_useful_connections`: under 1 MB 1, 1–10 MB 8, from 10 MB the full 64.

The setting and the 256 KB minimum piece still bound it, and the adaptive controller still climbs only while it
pays.

Tests: `planner::tests` (100 MB with 64 → ceiling 64; 12 MB → 48 by piece size), `profiles::tests`,
`profiles.test.ts`. Rust 724 passed.

## Arabic: no stray sideways scrollbar (October 2026)

`desktop-redesign.mjs`: in Arabic, no element that can scroll sideways is wider than itself, on the dashboard and in
Settings. Without the fix it fails (`main 1116/1106`); with it, it passes (3 runs under CPU load).

## Queue percentage and the name of a waiting duplicate (October 2026)

Found while taking the 1.0.0 screenshots. `formatPercent` (`format.test.ts`): one decimal everywhere; the queue's
rows showed the raw number. `desktop-smart-queue.mjs`: a running row's percentage has one decimal (fails on the old
code with `1.1461764574050903%`). `duplicates.rs`: a never-started download is named by its link in the answer and
in the Check link warning (failed with `""` before), and never by a link holding a token.

## Drop-down lists in the theme's colours (October 2026)

See `docs/ui-performance.md`. Tests:
* `selectMenu.test.ts` (4);
* the drop-down checks in `desktop-ui.mjs` (light and dark: the app's own list, its colour equal to the theme's;
  Escape; mouse pick; keyboard pick);
* closed fields (`fix_select_field_theme`): every `<select>` has no system look, a `select.input` has the surface of
  the text fields and its two-gradient arrow, light and dark. Without the fix it fails (`appearance: auto`, no
  arrow), with it it passes.

Results (Linux): vitest 126. e2e: `desktop-ui` passed 3 runs in a row; `throttle`, `smart-queue`, `views`, `media`
and `intelligence` pass. The screenshot `docs/screenshots/select-menu-dark.png` shows the list open. Two test-only
lessons:
* a screenshot with a size resizes the window, and a resize (correctly) closes an open list, so the suite waits for
  it to settle and takes this screenshot without a size;
* `scrollIntoView` on a row could scroll the page and close the list. The menu now scrolls only itself, and closes
  on a scroll only if the field really moved.

## Linux packages: media plugins (October 2026)

| Test | Checks |
|---|---|
| `tests/security/policy.test.mjs` "linux packages: ..." | no GStreamer / codec package in the `.deb` / `.rpm` Depends (still recommended); the build script sources `appimage-media.sh` and gives the bundler only the staged folder; staging a fake folder with hardware and software plugins keeps exactly the software ones; the deny rule covers the hardware names; a missing required plugin is reported |
| `apps/desktop/src-tauri/src/media_env.rs` (2) | the AppImage gets its own GStreamer cache (XDG cache folder, relative one ignored); nothing changes outside an AppImage, when the user set a cache, or without a home folder |

Checked by hand on Debian 13 (GStreamer 1.26), no AppImage rebuild:
* `scripts/appimage-media.sh stage` keeps 44 of the 273 system plugins;
* with only them (`GST_PLUGIN_SYSTEM_PATH_1_0`), `gst-launch-1.0 playbin` plays MP4 / MOV (H.264, HEVC, AAC),
  WebM (VP8, VP9, AV1), MKV, AVI, MPEG-TS, MP3, M4A, WAV, FLAC, Ogg and Opus. A folder without them fails, as
  expected;
* the existing AppDir with its plugins cut down to the same 44 plays them through its own bundled libraries, and
  the app starts from it;
* `desktop-preview.mjs` passes (22 checks, "it plays while downloading" included) with the app restricted to them;
* `apt-get -s install` of a package that *requires* an uninstallable codec fails ("held broken packages" class);
  the same package that only *recommends* it installs.
* Mutation: the old linux config makes the policy test fail; a hardware plugin added to the list is still kept out.

## Cards no longer flash when a download starts (issue #20, October 2026)

| Test | Checks |
|---|---|
| `tests/e2e/desktop-card-flash.mjs` (11) | a realistic list (3 finished, 2 paused); a paused download resumed and a new one added from the bar on the dashboard, then cards changing section on Active, with the server's speed swinging: the scenario really moves cards; no card already there replays its entrance; each stays at full opacity (sampled every frame); the new card fades in once; the grid's lit cells never restart and never jump in their beat; the pulse still follows the speed; no page errors |
| `apps/desktop/src/cardFlash.test.ts` (5) | an item is drawn with its entrance once; the fallback outlasts the animation; cards and queue rows take the class from the hook; `retime` changes the pulse only, from the stylesheet's base; the duration is never set inline |
| `tests/e2e/desktop-redesign.mjs` | "lit cells pulse ..." now reads the pulse's playback rate |

Proof: on the code before the fix the new e2e fails 4–5 checks (moved cards at 0.31 opacity, 108–371 grid jumps)
and the unit tests 3 of 5; after it, 11 runs of the e2e in a row pass.

## Sign-in in the browser confirmation (October 2026)

| Test | Checks |
|---|---|
| `tests/integration/tests/pending.rs::a_sign_in_typed_in_the_confirmation_is_used_and_never_shown` | the check finds `authRequired`; a password without a user name, `:` in the user name and a control character are refused and the request keeps waiting; `Debug` hides the sign-in; with it the protected file downloads byte for byte; the UI's data never holds it |
| `pending.rs::without_a_sign_in_the_protected_download_fails_asking_for_one` | no sign-in: the download fails with `needs: signIn` (the card and details offer Sign in) |
| `tests/e2e/desktop-browser.mjs` (+6) | the real native host hands over a protected link: the popup's sign-in part opens by itself and says why; a password alone is pointed out and the popup stays; typing the user name clears the message; the sign-in downloads the file; the list never shows it; a bad sign-in in the IPC answer is refused |

Also run without a regression: `desktop-ipc`, `desktop-usability` (Sign in from the card), `desktop-intelligence`,
`desktop-views`, `desktop-ui`, `desktop-card-flash`, and every Rust test.

## `desktop-diagnostics.mjs`: Retry found by its id (October 2026)

The suite failed at "second attempt" since "Use a new link" (`feat_session_link_refresh`): it pressed
`[data-testid=detail-error] button:last-child`, which now matches the Doctor panel's "Use a new link…" before the
Retry button. The app was right; the test pressed the wrong button. The Retry button of the details' error box has
its own id (`detail-retry`) and the test uses it: 3 runs in a row pass.

## Slow servers (issue #21, October 2026)

| Test | Checks |
|---|---|
| `tests/integration/tests/slow_server.rs::fewer_connections_never_cut_a_transfer_only_requests_still_waiting` | a server serving one request at a time (`State::one_at_a_time`): the count goes up, brings nothing, goes down; the requests given up were all still waiting (`State::cut_after_chunks` < 3 chunks), no transfer cut, no retry, file exact |
| `slow_server.rs::parts_that_start_together_check_their_joins_without_extra_requests_at_the_end` | 16 fixed connections, parts started side by side: no 2 KiB "final look" request, file exact |
| `slow_server.rs::a_slow_servers_broken_answer_continues_where_it_stopped` | a server slow to answer breaks its answer halfway: 2 GETs, the second from the saved bytes less 1 KiB with `If-Range`, < 1.1× the file sent, exact |
| `slow_server.rs::a_slow_server_that_cannot_continue_starts_again_from_zero_and_is_not_asked_again` | `/norange/`: asked once for the rest, refused, then from 0 (never asked again), exact |
| `slow_server.rs::a_file_changed_on_the_server_after_the_break_is_never_joined_to_the_saved_part` | the file changes (same size, new ETag) between the break and the rest: the new version whole |
| `adaptive.rs::the_downloads_own_lower_limit_stops_new_and_waiting_requests_never_a_transfer` | soft lowering: transfers stay, waiting requests and new ones give way; `set_limit` cuts transfers; `keep` follows raises |
| `adaptive.rs::a_request_waiting_for_its_answer_is_told_when_it_is_one_too_many` | `until_over_limit` waits within the limit, wakes on a lower one, for exactly the extra one |

Proof: on the engine before the fix, 4 of the 5 integration tests fail (a transfer cut after 12 chunks, joins
fetched at the end, the broken answer restarted from 0, and the range asked again). The 5th guards integrity.
`diagnostics.rs` (two 503s counted as two retries) caught a race in the first version: an answer already there is
now read before giving way. Benchmark: `docs/slow-servers.md`.

## Updates (Phase 19, October 2026)

| Test | Checks |
|---|---|
| `crates/update-verify` (10 unit) | signature accepted / any byte changed / no context / another key, even with the trusted id / malformed or upper-case signature / no keys / weak key / two keys (rotation); package rules (http, other host, other file, query, `..`, extension, system, duplicates, sizes, upper-case hash); plain http only for local tests; strict versions; no downgrade; replay refused; exact size and hash; size limits |
| `crates/update-verify/tests/node_interop.rs` (3) | a manifest signed by `scripts/update-key.mjs` (Node's Ed25519) passes the Rust check; changed or CRLF bytes do not; the throwaway test key is not trusted by the app |
| `tests/integration/tests/update.rs` (7) | a local release server: found, downloaded, verified, handed over, changed on disk refused; a tampered package never kept; a longer one cut at the signed size; foreign and missing signatures; no downgrade and no replay (across restarts); an interrupted download continues with a range and is verified; no cookie / credential sent; the automatic setting kept; a copy without keys sends nothing |
| `crates/system-integration` `update` (4 unit) | a file replaced only by exactly the checked bytes (0755, nothing left on failure); packages checked before opening; only a plain https page; a test binary is not an installation |
| `crates/download-manager` `update` (2 unit) | install wording per kind; the update folder keeps only the current package |
| `apps/desktop/src/update.test.ts` (3) | the status line for every phase and refusal (no network text); the store keeps the newest state. `locales.test.ts` now also refuses Markdown bold (`**`) in every locale (it showed as asterisks) |
| `apps/desktop/src-tauri` `update_tests` | the shipped keys file parses; the fixed addresses; the app's version is `x.y.z` |
| `tests/security/policy.test.mjs` "updates are only our own signed design" | no Tauri updater; key lines only; signature before parsing; verified before rename and install; Windows share lock; check before opening; https page only; no `process.env` in the key tool; no private key in the repository; fixed feed; no command takes an address or path |
| `tests/e2e/desktop-updates.mjs` (18 checks) | the real app, a local server, a throwaway key: a foreign signature refused in words; a signed version in Settings and the status bar; downloaded, verified, byte for byte; install handed over (dry run); the status bar opens Settings > Updates; the setting kept; requests carry no cookie or credential, only `VeloxFetch/<version>` |

The key tool was driven in a pseudo-terminal (`generate`, `manifest`, `sign`, `verify`; wrong, short and piped
passphrases refused); its output signed the interop fixture.

`desktop-usability.mjs` counts 10 Settings sections since Updates came (it counted 9); `desktop-diagnostics.mjs` reads
every on-screen status for "error" / "cannot", so the Updates panel says "Updates are not offered in this copy"
when a build has no update key. All 20 desktop suites pass on Linux.

## Performance (Phase 20, October 2026)

| Test | Checks |
|---|---|
| `tests/bench` (`velox-bench run`, by hand) | MB/s, CPU, peak RAM, DB time and time after the last byte for sizes × connections; every file SHA-256-checked against the server's pattern (a mismatch stops it). Results: `docs/benchmarks.md` |
| `tests/bench` unit (2) | the pattern depends on the position and any range equals the same bytes of the whole; sizes with units |
| `downloader-core` `verify` (4 new) | running hashes continued over three checkpoints equal a full read; progress that went back or a restart reads from the first byte again (a rewritten byte is in the hash); another plan's range never reuses a running hash; a later checkpoint on another copy of the record reuses this run's hashes without reading, and not after a restart |
| `tests/integration/tests/reverify.rs::hashes_built_while_downloading_are_exactly_the_hashes_of_the_bytes` | a real download paused part-way: every saved segment hash equals the SHA-256 of the server's bytes for that range |
| `tests/e2e/desktop-perf.mjs` (2 new checks) | 200 IPC calls from the page: p95 < 20 ms idle, < 30 ms with downloads running (measured 1 ms) |

## Release candidate 1.0.0 (Phase 21, October 2026)

| Test | Checks |
|---|---|
| `tests/e2e/desktop-i18n.mjs` | French and Arabic: every page, every Settings section, the Add and Batch dialogs and a failed download's details have no English words (links, paths, file names, the product name and log lines left out) |
| `apps/desktop/src/timeline.test.ts` (3) | every kind of timeline line the manager writes is said in French and Arabic without English left; English unchanged; unknown lines kept |
| `apps/desktop/src/messages.test.ts` (2) | every `ManagerError::invalid("…")` in `crates/download-manager/src` is in `messages.ts`; a refusal is said in French / Arabic; English and unknown messages unchanged |
| `apps/desktop/src/errors.test.ts` (2) | video job errors by code; the Queue page's failed download in the user's language |
| `download-manager` `video_failures_get_a_stable_code_for_the_ui` | DRM, live, playlist, sign-in, no video, site, remux, disk full, other file errors |
| `tests/security/policy.test.mjs` "one version everywhere" | Cargo, Tauri, UI, extension, lockfiles and manifests say the same `x.y.z` |
| `tests/e2e/desktop-diagnostics.mjs` | reads the version from `Cargo.toml` (it expected `0.1.0`) |

## Publishing kit (October 2026)

| Test | Checks |
|---|---|
| `tests/release/publish-kit.test.mjs` (6, in the full security gate) | shipped names and versions; staging every system with one checksum list, filled notes and README, pictures copied; refusals (damaged, missing, unknown, old version, conflicting, extensions only, a folder that is not a kit, never emptied); preflight (no key, untrusted key, missing or wrong key file, key inside the project, versions that disagree); the built app (no key, debug build); **the whole `publish-release.sh`** in a throwaway copy of the project with a throwaway key typed at a pseudo-terminal (`tests/release/pty-run.py`): refused without a key and with uncommitted changes, then Linux + a merged Windows folder staged, signed, verified with the app's key, every file in `SHA256SUMS.txt`, no passphrase or key anywhere |
| `tests/security/policy.test.mjs` (+1, and the update test) | the app, `update-key.mjs` and `publish-release.sh` name the same public repository; the script needs a terminal, never passes a passphrase, only drafts, never pushes or tags, always the full gate; the kit reads no environment variable; `*.sh` keep LF |
| `tests/e2e/publish-screenshots.mjs` | the 11 public pictures from the real app (1280×800), the version shown is the one that ships, no picture shows `127.0.0.1`. Taking them found three bugs, fixed on their own branches: grey closed drop-downs on Linux, the queue's raw percentages and the empty name of a waiting duplicate, a stray sideways scrollbar in Arabic |

Mutation check: dropping `--draft`, the terminal check, or the full gate from the script each fails the policy test.

Found by the whole-script test under load (3 failures in 5 runs while `cargo test` compiled): `update-key.mjs` showed
its passphrase question before turning echo off, so a passphrase typed at that instant was echoed. Echo now goes off
first; 8 runs of 8 under the same load pass.
