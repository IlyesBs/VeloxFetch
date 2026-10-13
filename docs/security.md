# VeloxFetch security model

**Assume every external input is hostile:** URLs, HTTP responses and headers, file names,
browser messages, downloaded files, update metadata and frontend IPC requests.

The app can reach the filesystem, the network and (later) browsers and OS APIs, so it is built
least-privilege, in layers. A failure in one layer must not be enough to compromise the system.

```
 Frontend ─► CSP ─► Tauri capabilities ─► IPC validation ─► app authorization ─► manager
                                                                         └─► engine ─► fs scope / network checks
```

## What is enforced today (Phases 1–7)

### Frontend / webview
* **Strict CSP** (`tauri.conf.json`): `default-src 'self'`, `script-src 'self'`, `style-src 'self'`,
  `connect-src ipc: http://ipc.localhost`, `img-src 'self' asset: blob:`. No `*`, no `unsafe-eval`,
  no `unsafe-inline`, no remote scripts or styles. The page cannot make its own network requests.
* `freezePrototype: true`, `withGlobalTauri: false`.
* The window's backdrop is a picture the page draws itself on a canvas and shows as a `blob:` image (`aurora.ts`,
  `docs/ui-performance.md`): no new source is allowed, nothing is fetched, and the CSP is unchanged.
* The drop-down fields' arrow is two CSS gradients (`styles.css`), not a `data:` picture: `img-src` stays as it is.
* The frontend build emits only external files (no inline script/style), which the CSP requires.

### Tauri capabilities (one per window, `apps/desktop/src-tauri/capabilities`)
* `main-window`: all 20 commands (the 11 download commands, `get_download_detail`, `list_history`, `get_metrics`, and the six queue commands) + event listening.
* `settings-window`: `set_speed_limit` only.
* `download-window`: inspect/control one download (incl. `get_download_detail`); cannot create, delete or list.
* Window permissions of the main window: `show` / `set-focus` (it starts hidden and the UI shows it once
  painted), and for the UI's own title bar (the window has no system one): `start-dragging` and
  `internal-toggle-maximize` (dragging the bar, double-click on it). The bar's minimize / maximize / close
  buttons and the frame theme go through our own `window_control` command, which acts on the calling window
  only (the generic `plugin:window|close` etc. stay unavailable, `desktop-ipc.mjs` checks it). `authz.rs` and
  `policy.test.mjs` list exactly these.
* The frame description given to the page (`window.__VELOX_CHROME__`, `lib.rs` `window_chrome`) is a frozen
  constant set by an initialization script with no input from outside (platform + surface name).
* No `core:default`, no fs / shell / http / opener / webview-creation permissions, no wildcard
  windows, no remote origins. Each command is its own `allow-<command>` permission (declared in
  `build.rs`).

### IPC surface
* **No generic commands.** There is no `execute`, `read_any_file`, `write_any_file` or
  `run_shell`; the UI can only call `create_download`, `start_download`, `pause_download`,
  `resume_download`, `cancel_download`, `retry_download`, `delete_download`, `get_download`,
  `list_downloads`, `set_priority`, `set_speed_limit`, plus the read-only `get_download_detail`,
  `list_history` and `get_metrics`, each taking ids or a structured DTO.
* The queue commands (`get_queue_state`, `set_queue_config`, `pause_queue`, `resume_queue`,
  `set_schedule`, `set_power_action`) are main-window only; the settings and download windows get none of
  them, and a test keeps the capability files and the Rust matrix identical.
* Requests are deserialized with `deny_unknown_fields` and typed enums, so extra or malformed
  fields fail at the boundary.
* **Per-window authorization in Rust** (`authz.rs`) repeats the capability decision. A unit test
  keeps the capability files and the Rust matrix identical, so neither can be widened silently.
* Errors returned to the UI are `{code, message}` with no internal paths or stack traces.

### Manager validation (`download-manager/src/validate.rs`)
* URLs: http/https only, host required, length and control-character limits. `file:`,
  `javascript:`, `data:` etc. are rejected.
* Destination: the UI can only pick a **relative sub-folder of the download root**. Absolute paths,
  drive letters, `..`, reserved Windows names and illegal characters are rejected; symlinks and
  junctions that lead outside the root are detected.
* File names must be bare names. Server-supplied names (`Content-Disposition`, URL) are reduced to
  a single safe path component by the engine.
* Category names, speed limits and checksums are validated; rejected requests are never stored
  and never touch the network.

### Engine
* Only http(s) redirects are followed (capped). `Accept-Encoding: identity` so byte ranges are
  meaningful. `Range` responses are validated against what was requested (status, `Content-Range`);
  `If-Range` guards against the file changing mid-download.
* **Languages:** the first-start language comes only from the web view's `navigator.languages`, and only a code
  matching `^[a-z]{2,3}(-[a-z0-9]{1,8})*$` that names a shipped file is used; Rust accepts only such codes for the
  tray (`valid_language`). Translations are plain text with `<b>` as the only markup (tested). The Windows web view's
  `--lang` argument exists only in debug builds started by the e2e harness.
* **Never lose a download** (`docs/never-lose-a-download.md`): a failed download is tried again on its own only for
  failures the network or the server may get over (never 401 / 403 / 404, disk, folder, integrity, web page), at least
  10 s apart and at most 20 times, with its own address and headers only; nothing is sent while offline; any user
  action stops the tries first; nothing survives a restart. Each try passes the usual run slots and resume checks.
* **Resume integrity** (`docs/resume-integrity.md`):
  * a ranged answer is refused if it is compressed, carries another strong ETag, or states another total size;
  * completed segments are hashed and re-verified before being trusted after a crash or an outside change of the
    `.part` file;
  * a broken segment map is never resumed or finished.
  * without a strong ETag, saved bytes are continued only when samples of them (up to 4 × 64 KiB, only bytes
    recorded as written, read inside the confined download folder) equal the server's; any contradiction starts
    over. The samples go to the download's own address with its own headers (no new host gets its cookies). A
    failed comparison never discards bytes and never continues them.
  * a one-connection transfer continued after a broken connection (issue #21) uses the new bytes only on proof
    of the same file: 206 from the exact offset, `If-Range` with the version, the same total in `Content-Range`, a
    strong ETag equal to the download's, and the last saved KiB sent again identical. Anything else starts again
    from byte 0. Two versions are never joined.
  * A segment hash proves only that local bytes did not change. End-to-end authenticity needs a strong validator or
    a trusted checksum, and is not claimed otherwise.
* Downloads land in a `.part` file and are renamed only after size (and checksum, if supplied)
  verification. A checksum mismatch deletes the file.
* **Checksum trust:** a checksum is only honoured when supplied by the caller. The engine never
  derives one from the same server response it is verifying.
* Memory use is bounded per worker regardless of file size.

### Power actions (Phase 6)
* **Opt-in and one-shot.** Nothing is armed by default; arming needs an explicit `set_power_action`, the
  armed state is never written to disk (a restart disarms it) and it is cleared the moment it fires.
* **Cancellable.** A countdown (default 60 s) is announced to every window before the action runs, and
  `set_power_action(null)` cancels it. New download activity aborts the countdown.
* **Conservative trigger.** It waits until nothing is running or queued, and never treats a user-paused
  download, or one held by the queue, as finished. Exiting the app never triggers it.
* **Fixed commands.** `system-integration` runs a fixed argument list with no shell; on Windows the tools are
  addressed by absolute path under `%SystemRoot%\System32`, so a hostile `PATH` cannot substitute them.
* **Testability without risk.** The executor is a trait; tests use a recording fake and the E2E test uses a
  dry-run executor that is only honoured by debug builds (`VELOX_DRY_RUN_POWER`). Release builds ignore it.

### Browser integration (Phase 7)

Assume the web page, other extensions and other local processes are hostile. Chrome itself warns that
content scripts run next to untrusted pages and that messages must be validated before they reach a
native application, so the design has **no content scripts**, and every hop validates:

| Layer | Enforced |
|---|---|
| Extension | Sender must be our own extension *and* our own popup page; closed message set; strict field lists; URLs are data (http/https only, no credentials, no control characters, ≤ 8 KiB); no `externally_connectable`, no `onMessageExternal`, no `eval` / `innerHTML` (a test scans the source); the only way out is `sendNativeMessage` (a test scans for `fetch`, `WebSocket`, ...). Permissions: http(s) site access and read-only `webRequest` for video detection (on by default; no `webRequestBlocking`, no `<all_urls>`, no `tabs` / `history`); cookies are still read only when the user turned on "send cookies", only for the link's own site and never from a private window. |
| Browser | Native messaging only reaches a *registered* host, and the manifest's `allowed_origins` / `allowed_extensions` name exactly our extension. |
| Host (`velox-native-host`) | Refuses to run unless started by a browser for an allowed extension id (parsed from its command line; a well-formed but unknown id, no arguments, or `--parent-window` alone all exit with code 2). Message size limit **64 KiB checked before the body is read**; commands limited to `ping` / `download`; unknown fields rejected; nothing is written to stdout except protocol frames. |
| Link | Mutual HMAC authentication (token file, fresh nonces, constant-time compare), caller allowlist, 128 KiB frames, 5 s handshake timeout, 8 concurrent connections, 30 requests / 10 s per connection, pipe squatting refused (`first_pipe_instance`). |
| App | Validates the message a third time, then the manager validates URL, file name (bare name, no paths) and headers before anything is stored. Errors returned to the browser are short codes, never paths or the offending input. |

Values that end up in HTTP headers are printable ASCII only (no CR, LF, NUL or other control characters), so a
hostile cookie or user agent cannot split or smuggle headers. `Referer` never carries a fragment or
credentials. Cookies are never accepted from a private window (the extension does not send them and the host
and app refuse them). Only `Referer`, `Cookie` and `User-Agent` can ever be attached to a download.
`Cookie` values are marked sensitive (never logged), are not forwarded across origins on redirects, and are
deleted from the database when the download ends.

**Limits and honest caveats.** The token file is protected by the user's profile permissions (mode 0600 on
Unix); a process running as the same user can read it, exactly as it could read the browser's own data.
Cookies are stored in the local database in clear text while a download is unfinished. The extension is
unsigned and not on any store; Chrome / Edge / Brave load it in developer mode and Firefox as a temporary
add-on until it is signed. The Firefox build has not been run in a real Firefox here; the Chromium build was
tested in a real Brave (see below).

### Lifecycle
* On exit, running downloads are paused and saved before the process ends.
* Crash recovery never trusts half-written state: it validates the `.part` file first.

## Not yet done (planned)

* Download-time scanning hooks (malware scan, file-type blocking) and a signed, verified updater
  have no design yet. (Phases 10, 11 and 12 are done: see below, `docs/threat-model.md` and
  `docs/release-process.md`.) (Phase 10, filesystem security, is done: see below. The download
  folder is still fixed; letting the user pick another folder would need the same checks applied to it.)
* Phase 12/19/30–31: signed releases, secure updater.

## Verifying

```
cargo test --workspace          # includes the capability/authorization/CSP consistency tests
node tests/e2e/desktop-ipc.mjs  # drives the real app: unknown IPC commands, hostile input, CSP
node tests/e2e/desktop-browser.mjs  # the real native host process against the real app: hostile input,
                                    # wrong origin / token, size and rate limits, registration (scratch key)
node scripts/security-gate.mjs                # the release gate: lint, tests, policy, audits, SBOM, hashes
node --test tests/security/policy.test.mjs   # the architecture policy alone (1 second)
cargo test -p path-safety -p velox-integration-tests --test fs_security   # names, links, planted files
cargo test -p secrets -p persistence -p velox-integration-tests --test secrets   # sealing, restart, no store
VELOX_TEST_OS_KEYRING=1 cargo test -p secrets os_store      # talks to the machine's real credential store
node tests/e2e/desktop-media.mjs   # streams (HLS) in the real app: dialog, download, cancel, refusals, routing
node tests/e2e/real-browser.mjs     # the built extension in a real Brave with a throwaway profile
(cd apps/browser-extension && npm test)   # extension validation, least-privilege and id-drift guards
```

## "Install extension" button (Settings > Browser integration)

* One command (`install_browser_extension`, main window only). Its only input is an optional browser *name* from a closed set; no path, URL or program name comes from the UI (unknown fields are rejected).
* It registers the native host for one browser (per user), copies the bundled extension into `<app data>/browser-extension/<chromium|firefox>` (plain files only, size/depth limited), then starts the browser with a fixed built-in page (`chrome://extensions`, `edge://extensions`, `brave://extensions`, `about:debugging`) and the system file manager on that folder. Programs are started with argument lists, never through a shell.
* Browsers do not allow silent extension installation, so the final "Load unpacked / temporary add-on" step stays with the user (until the extension is published in the browser stores).
* Cross-platform: default-browser and browser-location detection exist for Windows (registry), Linux (`xdg-settings`, `PATH`) and macOS (LaunchServices, `/Applications`); the mapping logic is unit-tested on every OS, the OS calls only ran on Windows so far.

## Confirmation before a browser download starts

* A handoff from the extension is parked (`PendingDownload`, in memory, max 50) and the window is brought to the front with a dialog showing name, server, address, size, type, resume support and target folder. Only "Start download" / "Download later" (`confirm_pending_download`, main window only, closed field set) creates a download; "Cancel" forgets it. Nothing is stored on disk while it waits.
* The same validation as any download runs when it is parked (URL scheme, file name, browser headers, no cookies from private windows), and again when it is confirmed (a renamed file is validated like any file name).
* The size/type shown come from one probe request made with the browser's referrer/cookies (same request the download would send first).
* The setting "Ask me before a download from the browser starts" is on by default; off = the previous behaviour (start at once).
* **Sign-in in the confirmation (October 2026).** A site that asks for a user name and password (HTTP authentication) can be answered in the dialog itself: its "Sign in to this site" part opens by itself when the check before the download found a `401`. The user name and password travel only in `confirm_pending_download` (closed field set), are checked before the request is taken (a mistake leaves it waiting: `:` or control characters in the user name, a password without a user name), and are then sealed with the download exactly like the Add dialog's. They never go back to the UI, into events or into logs (`ConfirmPending`'s `Debug` prints `***`). They are sent the same way as credentials typed in the Add dialog, under the same redirect rules.
* The extension drops the browser's own copy immediately (so the browser's "Save as" window and copy do not linger); if VeloxFetch cannot take the link, the browser downloads it again itself.

## Phase 12: security engineering (hardening gate)

The full analysis is `docs/threat-model.md` (15 threats, each with defence, test and residual risk); the release procedure is `docs/release-process.md`.
What Phase 12 added to the code and the build:

| Area | What | Where |
|---|---|---|
| SSRF-like abuse | a redirect from a **public** address into loopback / private / link-local (incl. `169.254.169.254`) / `.local` is refused; the same rule for the segment links of a stream | `http_client::redirect_decision`, `crosses_into_local`; `media_engine::resolve_url` |
| Malicious files | every finished file is marked as downloaded from the internet: Windows `Zone.Identifier` (ZoneId=3, origin without query or credentials), macOS `com.apple.quarantine`; `DownloadConfig.mark_of_the_web` (default on) | `path_safety::mark_downloaded` |
| DLL hijacking | first thing in `main` of the app, the native host and the CLI: current directory and `PATH` removed from the DLL search (Windows) | `path_safety::harden_process` |
| Untrusted input | deterministic mutation / property tests over every parser that sees hostile data: HLS, DASH, headers, URLs, batch patterns, bridge messages, file names (tens of thousands of cases each, plus nesting and size bombs) | `robustness` test modules |
| Architecture policy | 23 checks that read the repo (Phase 13 added: crash hook never in a shipped build, in the source and in the built programs, every crash point tested, no archive library, the test suites exist): no TCP/UDP listener, strict CSP, explicit capabilities, no plugin / eval / fetch in the UI, lockfiles, crates.io only with checksums, banned crates, CI pinned and locked, no updater, hash match, DLL hardening present, no shell, no secrets in log lines | `tests/security/policy.test.mjs` |
| Supply chain | `deny.toml` (licences, bans, sources, advisories), `.cargo/audit.toml`, `npm audit`, CycloneDX SBOMs, `publish = false` on every crate | `scripts/security-gate.mjs`, CI |
| Build pipeline | GitHub Actions on Windows, macOS and Linux: fmt, clippy `-D warnings`, tests, policy, audit, deny, npm audit, SBOM, build, SHA-256, then a signing job that refuses to publish unsigned | `.github/workflows/ci.yml`, `build.yml` |
| Formatting | `rustfmt.toml` added and the code base formatted once, so `cargo fmt --check` can be enforced | `rustfmt.toml` |

**Audit result today:** `cargo audit`: 0 vulnerabilities; 7 warnings, all unmaintained / unsound crates inside Tauri's GTK3 / URL-pattern stack, each listed with a reason and re-checked at every Tauri upgrade.
`cargo deny`: advisories, bans, licences and sources ok. `npm audit`: 0 vulnerabilities (UI and extension).

**Not done (needs decisions or credentials only the owner has):** code signing (Authenticode, Apple notarisation), a GPG signature over `SHA256SUMS.txt`, an updater
(deliberately absent, see the release process), malware scanning hooks, per-file-type blocking, a resolver-level check for DNS names that resolve to private addresses.

## Phase 13: testing

`docs/testing.md` maps every item of the roadmap (unit, integration against controlled servers, crash, security) to the tests that cover it, explains how the process-kill
crash tests work (a `failpoint` crate that is an empty function in every shipped build), and lists the defects the new tests found and that are fixed:
Pause / Cancel waiting for a slow server, no free-space check before pre-allocating (a server announcing a petabyte), and `Retry-After` given as an HTTP date being ignored.

## Smart throttle and Live preview (before Phase 15)

See `docs/smart-throttle-and-preview.md` (threats 21 and 22). In short: the preview protocol answers the main
window only, takes a download id only, serves only audio / video bytes that are really written, with `nosniff` and
a `default-src 'none'` CSP, and scripts still cannot read it (`connect-src` unchanged, `media-src` only for it).
The process scan reads names and executable file names only (never folders, command lines or environment), stores
and sends nothing, and any failure means full speed within 15 s (a gaming policy in force is kept that long, so a
single failed scan does not unleash traffic over a game; it can never stay longer).

**Gaming policy (October 2026):** two new optional fields on `set_smart_throttle` (`gamePolicy`: one of three
values, read strictly; `gameLimitBps`: 32 KB/s - 1 GB/s, so never zero = unlimited); no new command, capability or
CSP change. The pause policy only uses the queue's own pause / resume (no process is touched: VeloxFetch never
terminates, suspends or injects into a game); its hold lives in memory, so a crash or restart can never leave
downloads held, and a download the user paused is never resumed by it. Policy test "smart throttle".

Found and fixed on the way (engine): the last chunk of every range was never charged to the speed limits, so with
a fast server no limit applied at all; and a connection waiting under a limit kept waiting after the limit was lifted.

## Video tools fetched by the app (before Phase 15)

YouTube and similar sites need yt-dlp, a JavaScript runtime (Deno) and, for HD, ffmpeg. VeloxFetch downloads them itself
(threat 25): from fixed official https sources, SHA-256 checked against the published checksum before use, one fixed entry out of
an archive, into `<data folder>/tools`; yt-dlp is refreshed after 14 days. Windows: yt-dlp's ffmpeg build (GitHub), gyan.dev as
fallback; macOS / Linux: ffmpeg.martin-riedl.de (release, then snapshot). Large files come in 8 parallel ranges, each in its own
part file kept when the download stops (the app closed, the network dropped) and continued next time as long as the server
still describes the same file (`<file>.id`: address, size, ETag); the joined file is still checked against the published SHA-256.
The **installer** runs the same code (`velox-desktop --install-video-tools`, no other argument) after copying the files: Windows
setup (NSIS hook `installer-hooks.nsh`) into `<install folder>	ools`, Linux deb / rpm (`packaging/postinst.sh`) into
`/usr/lib/velox-fetch/tools`. It never fails the installation; the app reads those copies (read-only) and puts refreshed ones in
the user's data folder. The uninstaller removes them (not during an update).

## Fixes from testing on other computers (before Phase 15)

* **The browser's helper could not start on a computer without the Visual C++ runtime**: `velox-native-host.exe` imported
  `VCRUNTIME140.dll` (the app links it statically through tauri-build; the host did not). It now links it statically too
  (`apps/native-host/build.rs`); the UCRT it still uses is part of Windows 10 and later. The extension now says "the helper could
  not start" for this case instead of "connection lost".
* **Black window**: (1) the window was shown by the page in a `requestAnimationFrame`, which never runs in a hidden window, so it
  only appeared through the 6 s fallback; it is now shown from React's first effect (about 0.5 s). (2) A third-party overlay
  (RivaTuner Statistics Server, `RTSSHooks64.dll`) crashed the WebView2 browser process in about 1 of 6 starts here, leaving a black
  window: threat 24, `webview_guard.rs`. (3) An error boundary replaces a blank page by an error message.
* **One copy of the app** (threat 23).
* **Disk tile stuck on "reading disk..."**: an idle app sends no ticks, and the first metrics request answered before the disk had
  been read; it now waits up to 2 s for the first reading, the UI refreshes the numbers every 5 s while idle, and a download folder
  that does not exist yet is measured through its nearest existing parent.
* **Video bar never appeared**: detection was opt-in (popup switch plus a permission prompt); it is now on by default, pages already
  open when it starts get the bar too, and a bar message that wakes the background script waits for the settings instead of being
  answered "detection is off". The takeover no longer answers the browser for a download it has already cancelled ("Invalid
  downloadId").

## Videos from the browser, and 64 connections (before Phase 15)

* **Download video bar** (threat 20 in `docs/threat-model.md`): on by default (it can be turned off in the extension popup); it uses
  `webRequest` (read-only) and http(s) site access, granted when the extension is installed. The background script lists, per tab, what the browser itself
  loaded: video files played by the page (`type: media`, at least 512 KB; pieces a player stitches together are ignored), one HLS /
  DASH description per host, and the page itself when it is a video page of a known site. The bar (`videobar.js`, registered with
  `scripting.registerContentScripts` only while detection is on, top frames only) asks for that list without links and picks an item by
  id; a closed shadow root and `isTrusted` checks keep the page from reading or pressing it.
* **In the app** a browser video always waits for the user in the video dialog (`PendingDto.media`); `analyze_media` / `start_media`
  take the waiting request's token, so the link and the browser's headers come from Rust, never from the UI. Closing the dialog
  answers "no".
* **Video pages through yt-dlp** (`media_engine::site`): fetched by VeloxFetch (above) or found like ffmpeg (absolute paths only),
  run with an argument array and `--ignore-config --no-exec --no-batch-file --no-playlist`, a fixed output name in a scratch folder
  inside the validated download folder, the link last after `--`; the quality and the file type (MP4 / MKV / WebM / MP3 / M4A / Opus)
  are closed sets mapped to constant options; referrer and user agent are passed, **cookies never are**. The result
  is published like a stream download (no overwrite, sandbox re-checked, Mark of the Web).
* **DRM**: Netflix, Shahid, STC tv, Prime Video, Disney+, OSN+, Max, Apple TV+, STARZPLAY, TOD and a few others are recognised by address
  and refused everywhere (extension, browser hand-over, typed link, download) with a plain sentence; formats yt-dlp marks as DRM are
  never offered, and encrypted HLS / DASH streams stay refused as before. VeloxFetch never tries to get around copy protection.
* **Connections**: up to 64 per download (`planner::MAX_CONNECTIONS`, also the most the setting accepts), started at 1-32 by size and
  doubled only while that makes the download faster; the existing throttling detection steps down when a server objects. HTTP/1.1
  only, so each connection is a real TCP connection. Stream segments: 16 at once.

## Phase 14: observability without secrets

Everything VeloxFetch records about what it does stays on the computer (threat 19 in `docs/threat-model.md`):

* **One logger** (`crates/diagnostics`), installed by the desktop app before anything else runs; no other logging, crash-upload
  or telemetry library is linked (policy test). Lines go to a ring in memory (Settings > Diagnostics) and to
  `<app data>/logs/velox.log`, rotated at 2 MB with three older files kept; on Unix the files are readable by the user only.
  Warnings and errors always, VeloxFetch's own `info` lines, and `debug` only with **Detailed logging** (remembered).
  Libraries (HTTP, TLS, the web view) are kept to warnings and errors. Crashes (panics) are written to the log, redacted.
* **Never logged**: `Authorization` / `Proxy-Authorization` / `Cookie` / `Set-Cookie` values, passwords, tokens, API keys, signatures,
  credentials, a link's user name and password, **every value of a link's query string** and its fragment. Two layers: a field named
  like a secret is masked whatever it holds, then every line is scanned (`Redactor`). The home folder becomes `~`.
* **Per-download diagnostics** (the details view): download id, status, error code (`http_404`, `disk_full`, `timeout`...),
  attempts and automatic retries, the last HTTP status, connections now / at most, average speed, time transferring, and a timeline
  of what happened (server answer, retries with their reason, throttling, resume or start over, pause, failure). Saved in the
  database (table `diagnostics`, schema v5, removed with the download); every text in it is redacted before it is stored.
* **One log line per run** with the same numbers: `download ended id=12 status=failed error_code=http_404 retries=3 http_status=404
  connections=8 avg_speed_bps=...`. The engine's own lines carry `download{id=12}`.
* **Reports** ("Copy" in the details view, "Copy diagnostics report" in Settings) are built in Rust from the redacted data and
  redacted again as a whole. The app report lists settings without the proxy address or user name, the latest failures with
  their codes and hosts, and the recent log. They only ever go to the clipboard, when the user asks.
* The command-line tool prints its errors redacted, and its log (`VELOX_LOG=info|debug`) through the same redactor; the native
  host redacts what it prints on standard error.

## Everyday features added before Phase 14

The roadmap was re-read against the app before Phase 14; these user-facing features were missing or hard to reach
and were added with the same rules as everything else (threats 17 and 18 in `docs/threat-model.md`):

* **Network settings** (Settings > Network): the system's proxy (default), no proxy, or a manual one (`http`, `https`,
  `socks5`, `socks5h`; credentials in their own fields, never in the address), and the user agent. Everything is
  validated and the HTTP client is built *before* anything is saved; the proxy password is sealed and never shown again.
* **Sign-in and request headers** in the Add dialog (user name / password, referrer, cookies, user agent), checked with
  the same rules as what a browser hands over and sealed with the download. A download whose server answers `401`
  offers **Sign in and retry**.
* **Show in folder / Open** for finished downloads and history lines: by id only; programs, scripts, shortcuts and web pages are only shown, never opened.
* **Speed limits per kind of file** (Settings > Speed); a download without a category of its own shares the limit of its kind.
* **Errors in plain words** (`download_manager::explain`), with the engine's message kept for the details view.
* **Smart Duplicate Detection** (`check_duplicate`, main window only, read-only, no network): it answers with the file
  name, ids, state, confidence and file state, never with the link, its query / signature, credentials or a path (a
  download not started yet is named by its link's last path part, made safe, else "download": never the link). A
  match never deletes, cancels, pauses or overwrites anything; the prompt's actions are the existing commands with
  their checks (`FileTarget` ids, download folders, programs never opened). A finished file is only looked at
  (`metadata`) inside the download folders, and never read or hashed. A taken file name is a separate "name
  conflict", never a duplicate. Details: `docs/smart-duplicate-detection.md`.
* **Same file names**: a second download of `setup.exe` becomes `setup (1).exe`; two downloads of the same name running at
  once never share a partial file, and a file that appears under the name while downloading is never replaced.
  (Before, the partial file was shared and the final rename could replace an existing file: fixed, see `docs/testing.md`.)
* Search, filters and bulk actions, the "already in your list" prompt, history search / removal (files untouched, lifetime total kept),
  paste or drop a link anywhere (it goes into the link bar; nothing starts by itself), keyboard shortcuts, a sectioned Settings page.

## Download folders and sorting

The user can change the default download folder (Settings), send a single download to another folder (Add dialog, browser confirmation popup) and have new files sorted into
`Video`, `Audio`, `Images`, `Documents`, `Archives`, `Programs` and `Other` (names editable). The security rules, in `docs/threat-model.md` threat 16:

* **No path ever comes from the web view.** `pick_folder` / `choose_download_folder` open the system's own dialog from Rust; the answer is validated by `path_safety::validate_user_folder` and the UI holds a token.
  Refused: drive roots, system and program folders, Startup / autostart / `LaunchAgents`, `.ssh` / `.gnupg` / `.aws`, the app's own data folder (and any folder containing it), anything not writable.
* **Every write is checked** against the set of folders the user chose (`path_safety::AllowedRoots`, shared by the engine, the media engine and the manager) right before it happens; links are resolved, so a swapped folder cannot redirect a download.
* Type folder names go through the same strict name check as a file name (no separators, no `..`, no reserved names, no duplicates).
* A file sent to a folder the user picked is saved exactly there; sorting only applies inside the default folders, and never when a sub-folder is given.
* Existing downloads are never moved; they stay valid (resume, delete) after the default folder changes.

## Phase 11: secrets and credentials

**Inventory: every secret the app can hold, and where it may live.**

| Secret | Where it comes from | At rest | In memory | In logs / UI |
|---|---|---|---|---|
| Cookies (and the referrer) a browser hands over | the extension, per download | **Sealed** (ChaCha20-Poly1305) in the `request_headers` column; the key is in the OS credential store. Deleted when the download completes or is cancelled. | while the download runs | never: `Debug` prints `***`; not in any DTO or event |
| Password typed in a link (`https://user:pw@host/...`) | the Add dialog | Taken **out of the link** first, turned into an `Authorization` header and sealed exactly like a cookie. The stored and displayed link never contains it. | while the download runs | never |
| Proxy credentials, `--auth` | the CLI only (the desktop app has no proxy setting) | never stored; `VELOX_PROXY` / `VELOX_AUTH` are read from the environment, and a warning explains why a password on the command line is a bad idea | for the run | not printed |
| Database sealing key | generated on first start (256 bits, OS random) | **only** in the OS credential store: Windows Credential Manager, macOS Keychain, Secret Service on Linux (`app.velox.fetch` / `database-key-v1`). Never a file. | as long as the app runs (zeroed on drop) | never |
| Browser bridge token | random per launch | a user-only file in the data folder (see the decision below) | while the app runs | `Token(..)` |
| UI state | - | `localStorage` holds only the theme (`velox.theme`); a test scans the UI source so nothing else can be added | - | - |

**No plaintext fallback.** If the credential store is unavailable (no Secret Service on a bare Linux box, locked keychain, 10 s timeout), the app starts anyway, the Settings page says so,
and cookies / credentials are kept **in memory only**: they work for the running session and are gone after a restart (a download that needed them then fails to resume instead of sending garbage).
There is no key file and no "encrypt with a key stored next to the data".

* **Sealed values** are `s1:` + hex(random nonce, ciphertext, tag). The column name is authenticated with them, so a value cannot be moved to another column. Wrong key, altered or truncated values are refused (`None`); a download with an unreadable secret loads normally, without it.
* **Old clear text is scrubbed.** A database written by an earlier version may hold cookies in the clear. At start, before any record is read, those rows are sealed (or, with no credential store, erased), then the write-ahead log is truncated and the file vacuumed. `PRAGMA secure_delete=ON` zeroes deleted values from then on. A test greps every byte of the database, its `-wal` and `-shm` files for the cookie before and after.
* **Logging.** `DownloadRecord`, `NewDownload`, `BrowserContext` and the bridge's `DownloadRequest` have hand-written `Debug` that hides cookies and shows links without query and fragment (signed links keep their token there). `secrets::redact_url` / `redact_headers` are the helpers for any new log line.
* **Redirects.** The `Authorization` and `Cookie` headers are marked sensitive and reqwest drops them when a redirect leaves the origin.
* **Streams** (`.m3u8` / `.mpd`) refuse links that contain a user name or password; they were never able to carry one safely.

**Decision: the bridge token stays in a file.** It is regenerated at every start, is only meaningful while the app runs, and the browser-launched helper must read it without any prompt. A credential-store entry would be readable by every process of the same user exactly like the file (Windows Credential Manager has no per-application ACL), so it would add failure modes (no store, locked keychain) and no isolation. What is protected instead: created with an exclusive open (no write through a planted link), mode `0600` on Unix, in the per-user data folder, never sent over the pipe (only HMAC proofs are), and never printed.

**Known limits.**
1. Anything that runs as the same user and can call the credential store can read the sealing key (this is what an OS credential store is, not a secure enclave). Sealing protects the database file when it is copied, backed up, synced or read by another account, which is the realistic threat.
2. Query-string tokens in a *link* are not treated as secrets (they are part of the address the user pasted and are stored with it); they are only kept out of logs and debug output.
3. Secrets that were in memory are zeroed where the key is held (`zeroize`), but cookie text passes through ordinary strings while a request is built.
4. macOS: the first use may show a Keychain prompt; "Deny" behaves like an unavailable store (memory only).
5. Linux was compile-checked only, never run.

## Phase 10: filesystem security

One crate, `crates/path-safety` (no network, no dependencies except `libc` on Unix), decides everything about file names and
links. The downloader, the media engine and the input validation all call it, so they can no longer disagree (before, there were three
slightly different copies).

| Threat | Rule | Where |
|---|---|---|
| `../../x`, `..\..\x`, absolute paths in a server-supplied name | only the last component after `/` **or** `\` is kept; `.`, `..` and empty become `download` | `sanitize_filename` |
| Windows-illegal characters | `< > : " \| ? *` become `_` | same |
| NTFS alternate data streams (`a.txt:evil.exe`, `x::$DATA`) | `:` is illegal, so it can never reach the filesystem | same |
| Reserved device names | `CON PRN AUX NUL COM1-9 LPT1-9`, the superscript forms (`COM¹`), `CONIN$` / `CONOUT$`, also with an extension or trailing spaces (`CON .txt`) get a `_` prefix | `is_reserved_device_name` |
| Trailing / leading dots and spaces (Windows silently drops them, so `a.exe.` collides with `a.exe`) | trimmed | same |
| Unicode tricks | control characters, zero-width characters, line / paragraph separators, the right-to-left override (`invoice<U+202E>fdp.exe` displays as `invoiceexe.pdf`), tag characters and variation selectors are **removed** | `is_invisible` |
| Very long names | at most 200 **bytes** (Linux / APFS allow 255 bytes, NTFS 255 UTF-16 units; the margin leaves room for `.part` and ` (99)`), cut on a character boundary, a short extension is kept | `limit_len` |
| Names chosen in the UI or by an extension | **rejected**, never repaired (same rules, plus: no leading space, no path separator) | `check_component` |
| Sub-folder escapes | relative only; `..`, drive letters and `:` rejected; the deepest existing ancestor is resolved (symlinks, junctions, `..`, case) and must be inside the download folder | `subdir`, `ensure_within` |
| A folder swapped for a link **after** the download was queued or paused | the engine and the media engine re-run `ensure_within` before creating the folder, before resuming, and before the final rename / publish; the manager hands them the download folder as `sandbox_root` | `Downloader::confine`, `media_engine::download::confine` |
| A planted symlink where the `.part` file goes (it would be written through or truncated) | the file is opened with `create_no_follow` / `open_write_no_follow`: a symlink at the last component is refused (a dangling one too); on Unix additionally `O_NOFOLLOW` (closes the check-then-open gap) and a file with several hard links is refused; before the final rename the `.part` is checked again | `path_safety` |
| Deleting through a link | "delete file" and part-file removal only run when the path is inside the download folder; removing a link removes the link, never its target | `delete_download` |
| Media: name collisions and overwrite | `create_new` (exclusive create, fails on any existing file or link), then rename, so an existing file is never replaced | `reserve`, `publish` |

Idempotence is tested: sanitising an already-sanitised name changes nothing, and every sanitised name passes the strict check.

**Known limits.**
1. On Windows the standard library has no `O_NOFOLLOW`, so a link swapped in during the few microseconds between the check and the open is not caught by that step (the folder re-check before the rename still is).
2. Windows file symlinks need a privilege to create, so the tests for them skip where the user lacks it (folder junctions, which need none, are always tested).
3. The checks run per operation; this is not a kernel-enforced sandbox. Someone who can already write inside the download folder as the same user is outside the model.
4. Risky file types (`.lnk`, `.scf`, `.exe`...) are saved under their real name; flagging them belongs to the download-scanning hooks planned for Phase 12.

## Phase 9: the media engine (HLS / DASH)

A stream description is **untrusted data**: whoever runs the server chooses every line of it. `crates/media-engine`, hard limits in `limits`:

| Rule | Where |
|---|---|
| Playlists / manifests are read with a hard cap (4 MiB) while streaming, not after; not valid UTF-8 or not a playlist / MPD is refused | `fetch_text`, `hls::parse`, `dash::parse` |
| Every URL inside a manifest: http(s) only (no `file:`, `ftp:`, `data:`, `javascript:`), a host, no user name or password, at most 4096 characters, no control characters, and **no https -> http downgrade** | `resolve_url` |
| At most 30,000 segments per track, 200 tracks per manifest, 32 GiB per piece, 64 GiB per job; byte ranges use checked arithmetic | `limits`, `byte_range` |
| XML: DTDs are refused (no entity tricks), nesting depth 32, at most 100,000 elements; URL templates accept only `$RepresentationID$`, `$Number$`, `$Bandwidth$`, `$Time$` and `$$`, with a bounded width | `parse_xml`, `expand` |
| Encrypted (AES-128, SAMPLE-AES), DRM-protected, live and multi-period streams are refused with a reason, never half-handled | `hls::parse_media`, `dash::parse` |
| Track ids from the UI must be plain alphanumeric ids from the analysis; the manifest is re-read and unknown ids fail. The UI never sends a URL to fetch | `start_media`, `download` |
| Files: only names built by the engine (a sanitized stem + fixed suffixes) inside the folder the manager validated (`validate::subdir`, no `..`, no symlink escape); the name is reserved with `create_new` and never overwrites (`name (1).mp4`); working files live in a hidden scratch folder that is always removed (done, failed, cancelled, shutdown) | `sanitize_stem`, `reserve`, `Scratch` |
| At most 3 stream jobs at once, 100 kept in the list; a job is cancelled at shutdown | `manager/media.rs` |
| Stream links never go down the ordinary file path: the confirmation flow and batches refuse `.m3u8` / `.mpd` links with a pointer to Stream download | `review_link`, `classify` |

ffmpeg (optional, `crates/media-engine/src/ffmpeg.rs`): `Rust -> validated arguments -> ffmpeg process`, never `format!("ffmpeg {}", ...)`.

* **No shell**: the program is started with an argument array; a file name, a title or a manifest can never become part of a command.
* **Found by absolute path** (next to the app, then absolute `PATH` entries): never through the current directory.
* **Absolute file arguments only**, so a path can never be read as an option.
* **The input format is forced** (`-f mpegts` / `mov` / `matroska` / `aac` / `mp3`). Without that ffmpeg guesses from the bytes, and a "video" that is really an HLS playlist or a `concat` script could make it open other local files or network addresses. `-protocol_whitelist file` limits it further.
* `-nostdin`, `-n` (never overwrite), stdin closed, `-c copy` (no decoding of untrusted media beyond what the demuxer needs), a 2-hour timeout after which it is killed, and it is killed on cancel.
* Tests: a stand-in ffmpeg records the exact arguments (`tests/integration/tests/media.rs`); with `VELOX_TEST_FFMPEG` set to a real ffmpeg, real HLS (TS and fMP4) and DASH streams are made, downloaded, joined and checked with ffprobe.

## Phase 8: batch downloads and clipboard detection

Batch (`crates/download-manager/src/batch.rs`, pure Rust, identical on every OS):

| Rule | Where |
|---|---|
| A pattern generates at most 1000 links, at most 3 ranges, at most 9 digits per bound; the product is computed with checked arithmetic **before** anything is generated | `expand_pattern` |
| A batch holds at most 1000 links; the pasted / imported text at most 512 KiB; a link at most 2048 characters (a file import is also capped in the UI, and re-checked in Rust) | `text_lines`, `classify` |
| Every link goes through the normal URL validation (http/https, host, no control characters); one thing per line | `classify` -> `validate::url` |
| Duplicates inside the batch and links already in the list (any state but cancelled) are skipped; "same link" ignores host case, default port and fragment | `normalize`, `known_links` |
| The destination sub-folder is validated once up front (no `..`, no absolute path or drive, no reserved names, no symlink escape); a bad folder fails the whole batch and adds nothing | `validate::subdir` |
| Exactly one of list / pattern; unknown fields are refused at the IPC boundary; a refused batch adds nothing | `BatchRequest` |
| The preview and the add both re-derive everything in Rust from the same request; the UI's preview is never trusted | `preview_batch`, `add_batch` |

Clipboard detection (`apps/desktop/src-tauri/src/clipboard.rs`, `crates/system-integration/src/clipboard.rs`):

* **Off by default.** Reading the clipboard only starts after the user ticks the box in Settings; the choice is stored in the database.
* Only text is read (never images or files). The text is checked *once* by `detect_download_links` and dropped: it must consist **only** of http(s) links, one per line (at most 50), each ending in a file extension from a fixed list; anything else (prose, passwords, a web page link, a link with a user name or password) is ignored. Nothing is stored or logged; the watcher only keeps a hash to notice changes.
* What was on the clipboard when detection was switched on is a baseline and is never reported; nothing is reported while VeloxFetch itself has the focus; links already in the download list are not offered again.
* A notice is shown; **nothing is downloaded until the user presses Download** (one link -> the usual confirmation with details and Start / Cancel; several -> the batch dialog with its preview).
* Cross-platform: `arboard` (Windows, macOS, X11, Wayland data-control). Where the OS refuses background clipboard reads (e.g. GNOME on Wayland) the watcher reports "unavailable" and the UI says so instead of failing silently.
* Tests read "the clipboard" from a file (`VELOX_CLIPBOARD_FILE`, debug builds only), so the real clipboard of whoever runs them is never touched.

## Phase 15: one door to the operating system

`system_integration::system::SystemIntegration` is the only way the download manager reaches the OS: power
actions, show / open a file, keeping the computer awake, notifications, the `veloxfetch://` link protocol.

* **Real only in the app.** `ManagerConfig::new` gives the dry run (`DryRunSystem`, which records and touches
  nothing); only the desktop app passes `SystemHandle::native()`, and debug builds switch back to the dry run for the
  end-to-end tests. A test that forgets to set it can no longer open windows or shut the machine down.
* **No shell, no text in commands.** Notification text is a separate argument (Linux: after `--`; macOS:
  AppleScript `argv`, never inside the script) or XML-escaped (Windows toast); control characters are removed and
  the length bounded first.
* **Keeping awake never traps the computer.** It only holds off the *automatic* sleep after inactivity (Linux
  `systemd-inhibit --what=idle`, macOS `caffeinate -i`, Windows `ES_SYSTEM_REQUIRED`); a sleep the user asks for, and
  the app's own power action, still happen. It is a handle released when nothing downloads, when the user turns it
  off, and at exit; the Linux / macOS helper also ends with the app if the app crashes (closed pipe, watched pid).
* **Link protocol.** Per user only (Windows `HKCU\Software\Classes`, Linux `~/.local/share/applications` +
  `xdg-mime`; macOS declares it in the app bundle). The scheme is checked (never `http`, `file`, `javascript`...),
  the program path must be absolute, and the desktop entry quotes it as its specification requires. Registering it
  and handling the links is wired in Phases 16 to 18.
* Policy test: `Phase 15: the operating system is reached through SystemIntegration only...`.

### Starting the app for a browser download

* The helper starts the app only for a validated `download`, never for a `ping`, at most once per session, and
  only a program it found itself: fixed places relative to the helper, or the absolute path the app wrote in its own
  user-only data folder (the folder that already holds the link token: whoever can write there is already the
  user). Nothing in a browser message chooses what runs.
* Inside a snap / Flatpak the helper only asks the XDG desktop portal to open the fixed link `veloxfetch://start`;
  the app ignores the link's content (it just starts, and a second copy leaves). The link is registered for the
  user only (`~/.local/share/applications`), only in release builds, and only while a sandboxed browser is
  connected; it is removed when none is.
* The started app runs in its own process group (Unix) or detached (Windows): ending the browser never ends it.

## Phases 16-18: Windows, Linux and macOS on the desktop

* **Tray / close to tray.** Closing the window hides it only when the user wants that *and* the desktop shows tray icons; on Linux the
  AppIndicator library is looked for before the tray code runs (that code stops the program when it is missing). No tray: closing quits.
* **Start at login** is off by default and only follows the user's switch: the Windows `Run` value (HKCU), an XDG autostart file
  (`~/.config/autostart`), a macOS launch agent (`~/Library/LaunchAgents`, `KeepAlive` false: never restarted after the user quits).
  Each runs only the app itself, quoted, with `--hidden`; the path is checked (absolute, no quotes or new lines). Debug builds never
  offer it. The uninstaller removes the Windows value.
* **Notifications** carry the file name only, cleaned of control characters and bounded; Linux uses the desktop's notification service
  over D-Bus (body escaped, servers read markup), then `notify-send`; Windows toasts escape XML; macOS passes the text as argv.
* **Keeping awake on Linux** falls back to logind's inhibitor over D-Bus ("idle" only) when `systemd-inhibit` is missing; the handle is
  a file descriptor released when dropped or when the app ends.
* **The window is shown only by Rust** (`window_control: ready`): the UI lost the `show` / `set-focus` window permissions.
* **A second launch** on Linux / macOS signals the running copy over a socket in the data folder (mode 0600; the user's private
  runtime folder when the path is too long). The connection carries nothing: it only means "show the window".
* **First start fix:** the browser link now creates the data folder before it binds, so the very first run after installing has browser
  integration and the one-copy guard.
* **Windows install:** per-machine into Program Files (admin-only writes), DLL search without the current folder
  (`path_safety::harden_process`), `veloxfetch://` registered by the installer and removed with it. No file associations: the app owns no
  file type, and taking one over (`.txt` lists) would hijack the user's files.
* **macOS:** hardened runtime with no entitlements; no App Sandbox (the app writes to folders the user picks and starts the checked video
  tools); `veloxfetch://` declared in the bundle's Info.plist.

## Smart throttle, automatic mode

* Signals: the computer's **total** download traffic (the same counters as the dashboard, minus VeloxFetch's own downloads) and
  the time since the last keyboard / mouse input (Windows `GetLastInputInfo`, macOS CoreGraphics, Linux GNOME idle monitor /
  screen saver service / logind's idle flag). Never which program or site uses the network, never any input itself; nothing
  is stored or sent.
* Safe: no counters means full speed; the cap never goes below the reserved minimum (32 KB/s at the least, 256 KB/s by
  default), so downloads never stop; it uses its own rate bucket, so the user's limits and the engine are untouched.
* Steady: smoothed readings, separate on / off thresholds, two readings to confirm before slowing, a calm period before
  speeding up (doubled when the network keeps changing), then steps of 1.5x.
* Measured from totals only (October 2026): the computer's received-bytes counter (`SysIo::net_received_total`, read on
  the watcher's blocking thread) and VeloxFetch's own received bytes, over the same window. Still no per-program, per-site
  or per-connection traffic (the policy test checks `sysio.rs`); nothing new is stored or sent.

## Download Doctor

* **Diagnosis only:** the Doctor never retries, starts, writes, deletes or requests anything. The engine's retry
  loop stays the only automatic recovery, so there is no second retry loop and no retry storm. The policy test checks
  the glue module for any of those calls.
* **No secrets:**
  * a `Diagnosis` holds codes, classes, counts and byte numbers only, never free text (checked by the policy test);
  * transport errors are read from their *causes*, never from the top message, which names the link;
  * the one timeline line goes through the usual redaction;
  * the Doctor logs nothing.
* **Integrity is kept:** a checksum mismatch is `critical`. The engine already deletes the part and never completes
  it; after a second identical mismatch the Doctor says retrying will not help.
* **Fix:** a refused certificate, or a server that does not speak TLS, is no longer retried
  (`http-client` `tls_will_fail_again`). Retrying it gave nothing but more connections to that server.

## Download Intelligence

* **Read-only:** the analysis creates, writes, moves and deletes nothing, and changes no setting. The policy test
  checks `manager/intelligence.rs` for any file-writing or settings call, and checks that the rules
  (`intelligence.rs`) do no I/O.
* **Bounded:** one `HEAD` and one `Range: bytes=0-0` request through the hardened client, with:
  * no retries;
  * a 20 s cap;
  * a 1-byte body read only when ranges are verified; any other body is dropped unread.

  Video pages and stream links make no request.
* **No secrets out:**
  * failures become a status or a stable code (never error text, which can contain the link);
  * the result has the final host, not the address;
  * the dialog's link goes through `display_url`, which hides the `user:pass@` password, secret-named query values
    and the values of signed links;
  * cookies and headers are used for the request only and are never in the result;
  * nothing is logged.
* **`analyze_link`:** main window only (`build.rs`, main-window capability, `authz.rs`). Its request is
  `{ url: string | null }` with `deny_unknown_fields`, so it cannot carry headers or cookies. Only `http` / `https`
  links are accepted (`validate::url`).
* **No bypass:** only the credentials the user or browser already gave are sent. A sign-in requirement is reported,
  never worked around.
* **Not a scanner:** the findings are transport and link facts (HTTP, redirects, sign-in, temporary link, space).
  The app makes no malware claims.
* **Cancellable:** answering the dialog aborts its analysis. A new check, `url: null`, an edited link or closing the
  dialog cancels the Add dialog's check.

## Smart Download Queue

* **Five commands**, main window only: `get_queues`, `save_queue`, `delete_queue`, `queue_action`, `place_download`.
  * They are registered in `build.rs`, the main-window capability and `authz.rs`; the detail and settings windows
    cannot reach them.
  * Requests are read strictly (`deny_unknown_fields` on `SaveQueue`, `PlaceDownload`, `QueueRules` and the tagged
    `QueueAction` / `StartWhen`).
  * They carry ids, a name, times and limits only: no path, no program, no command.
* **Checked in Rust before anything is saved:**
  * times are `HH:MM`, and start / stop must differ;
  * at least one day is chosen;
  * names have at most 40 characters, with control characters removed; there are at most 16 queues;
  * the busy limit is at least 64 KB/s and the big-file size at least 1 MB;
  * a download's start time is `YYYY-MM-DDTHH:MM`, at most a year ahead;
  * the main queue cannot be deleted.

  Saved rules that do not pass these checks are ignored at the next start.
* **The connection is read, never probed:**
  * Linux reads the routing table and `/sys/class/net`;
  * Windows uses `GetBestInterface` and `GetIfEntry2`;
  * macOS uses `SCNetworkReachability`.

  No socket is opened and nothing is sent; the policy test forbids sockets and requests in `network.rs`. Only
  "online" and "Wi-Fi / cable / mobile / other" are kept, in memory. Idle time is the same reading Smart Throttle
  uses: a duration, never any input.
* **The queues never reach the power action**, the global pause or the global schedule (policy test).
* **What the computer cannot tell never holds a download back.** That covers unknown idle time, an unknown or VPN
  connection, and unmeasured traffic. A rule can therefore not be turned into a way to stop downloads by hiding
  information.

## Fixes after Phase 18: Linux live preview, gaming policy additions (October 2026)

* **Linux private preview address** (`src-tauri/src/preview_server.rs`, `loopback`): WebKitGTK's player cannot read
  the `velox-preview` protocol, so on Linux the same bytes are served on `127.0.0.1:<random port>`, built to rule 12.2:
  a 64-hex secret from `/dev/urandom` in every path (new each launch, never logged or stored), the `Host` checked
  (DNS rebinding), an `Origin` other than `tauri://localhost` refused, GET / HEAD only, ids only, the same reader as
  the protocol (only written audio / video bytes inside download folders), request heads of at most 8 KB read within
  10 s, at most 16 connections and 40 new requests a second (429), no CORS headers. The CSP gets that exact origin in
  `media-src` at start-up (no wildcard; the static `tauri.conf.json` is unchanged and `csp_is_strict` still holds);
  `connect-src` is unchanged, so scripts cannot read it (checked in `desktop-preview.mjs`). New command
  `get_preview_base` (main window only; no arguments; `None` on Windows). Threat 32; policy test
  "no shipped code opens a TCP or UDP listening socket, except the Linux / macOS preview address built to 12.2".
  **Since the preview fix (October 2026) macOS uses the same address** (WKWebView's player cannot stream a growing
  file through the custom protocol). Same rules, same code. `Info.plist` allows plain http for local addresses only
  (`NSAllowsLocalNetworking`); `NSAllowsArbitraryLoads` and per-domain exceptions are forbidden by the policy test.
  A running download is answered as a growing file (headers at once, bytes as written): still only written
  audio / video bytes, the same reader.
* **Program list paths**: an entry may be an absolute executable path (no wildcards, at most 260 characters). The
  scan reads executables' full paths only while such an entry is listed; they are compared and dropped, never
  stored, logged or sent (policy test "smart throttle").
* **Gaming pause also pauses video jobs** through their own pause / resume; in memory only, so a restart never leaves
  one held.
* **Browser registration in tests on Linux / macOS**: `VELOX_NMH_SCRATCH_HOME` (debug builds only, like
  `VELOX_NMH_SCRATCH_KEY` on Windows) puts the browsers' manifest folders under a scratch home; release builds never
  read it.

## Steady speed and Smart throttle fixes (October 2026)

No new command, permission, setting, file, listening socket or network destination. What changed on the wire:
* **Fewer requests to servers that refuse**: after a "too many connections" answer (429 / 503) the download sends no new
  request for 1 s, then 2, 4… up to 16 s while the server keeps refusing (bounded, reset by the next accepted request),
  instead of up to 40 refused requests in a row. A long `Retry-After` still delays the refused part itself (capped at
  300 s as before), never the whole download for longer than 16 s.
* **Runs**: one ranged request may now cover several consecutive parts (at most 16) of the same file; the answer is
  checked as before (`206`, a `Content-Range` that matches the request, the strong validator / `If-Range`), and its
  bytes are written only into the parts that request claimed.
* **Under a speed limit**, requests ask for a few seconds of data and carry `Connection: close` (a standard header; no
  information about the user). The connection count drops to what the limit needs.
* Window: `<html data-throttle>` is written only when it changes; no new data reaches the page.

## Download Profiles (October 2026)

* **Presets only.** A profile can set the global speed limit, downloads at once, connections per download and Smart
  throttle's automatic mode, nothing else. Paths, TLS, integrity checks, credentials, network settings and the
  browser link are not profile fields. `deny_unknown_fields` refuses anything else.
* **Checked in Rust** (`profiles::validate_settings`, `validate_name`): the same ranges as Settings, names 1–40
  characters without control characters, at most 20 of your own. An invalid request changes and saves nothing.
* **Lowest limit wins.** A profile only sets the limiter's global layer: per-download and per-type limits, Smart
  throttle's cap and its reserved minimum are separate layers it cannot lift.
* **No surprise actions.** A profile never starts, resumes, pauses, cancels or restarts a download, and never
  touches partial files.
* **Bad saved data** (unreadable, newer, partly invalid) never stops the app. Default (your settings) is used, and
  no other setting is changed.
* **IPC:** `get_profiles` / `profile_action` are main window only (`authz`, capability file, build manifest,
  destructive-commands test list).

## Use a new link (October 2026)

Full rules: `docs/new-link-recovery.md` §6.
* **`use_new_link`** is main window only (`authz`, capability file, build manifest, destructive-commands test list).
* **Credential isolation:** a pasted link gets the download's cookies / `Authorization` only on the same origin
  (scheme, host, port). The referrer and user agent are kept. A browser hand-over uses the browser's headers for that
  link. Credentials in a pasted link become a sealed header, never part of the stored link.
* **No corruption by design:** saved bytes are continued only on proof of the same file (strong ETag + size + matching
  byte samples), or "likely" with the user's explicit confirmation. Another file is never applied. A link without
  working ranges or without a validator never continues saved bytes. Final size and checksum checks are unchanged.
* **No secrets in logs:** the request's `Debug` redacts the link, and diagnostics events never contain it. Reports
  pass the redactor (tested with a signed-URL secret).
* **Paths:** the partial file is read for samples only after `AllowedRoots::check`. Nothing is deleted.
* **Bounded:** one attempt per request, no retries, one check per download at a time, the same link and session
  refused.

## Download again (October 2026)

* **`redownload`** is main window only (`authz`, capability file, build manifest, destructive-commands test list).
  It takes only an id.
* **Paths:** it deletes only the record's partial file, after `AllowedRoots::check`. The new download's folder is
  the old one only while it is still allowed; otherwise it is the download folder. The engine confines every path
  again before writing.
* **Never overwrites:** a file at that path keeps its content; the new copy takes a free name.
* **No surprise actions:** refused for running, paused, waiting and not-started downloads, with no change.
* Browser cookies are not brought back (they were cleared when the download ended).
* `file_missing` uses `symlink_metadata`: a link is never followed.

## Video download details (October 2026)

* No new command or capability. The page reads the job list the main window already gets, and its buttons call the
  existing media commands.
* It shows the job's own link, folder and file names, as the list does. No tool names (checked by e2e).

## Dependency advisories (October 2026)

* `source-map-js` 1.2.1 → 1.2.2 in the desktop UI's lockfile (GHSA-68fv-2mgg-jv7q: event-loop denial of service
  through crafted source-map offsets). It is a build-time dependency of the bundler (Vite / Tailwind), never shipped
  in the app. `npm audit` is clean for the desktop UI and the browser extension, and the release gate
  (`scripts/security-gate.mjs`) passes in full.

## Linux packages: install and start on every system (October 2026)

* The `.deb` / `.rpm` only **recommend** the GStreamer packages. A system whose codec libraries are pinned or come
  from mixed repositories can no longer be left with a half-configured package (`apt` "held broken packages").
* The AppImage carries an **allow-list** of software playback plugins (`scripts/appimage-media.sh`). Hardware and
  driver plugins (VA-API, NVIDIA, Intel QSV, V4L2, Vulkan, KMS), capture devices and network-service plugins
  (`soup`, `curl`, `rtsp`, `webrtc`) are never carried: they ran another computer's driver code inside the app
  and crashed it. Less code carried also means less code that can be reached through a crafted media file.
* The AppImage's GStreamer uses its own plugin cache (`media_env.rs`); a cache path the user set is kept. Nothing
  else in the environment changes, and installed packages are not affected.
* Checked by the policy test "linux packages: ...": no media package is required, the build script hands the
  bundler only the staged folder, and a staging run on a fake folder keeps only the allowed plugins.

## Cards no longer flash when a download starts (October 2026)

* UI only: how cards animate (`ui/entrance.ts`, `ThreadMatrix.tsx`). No command, permission, data or network change;
  no style is injected (the CSP still forbids it: the animation speed goes through the Web Animations API).

## Updates (Phase 19, October 2026)

* **Who can ship an update:** only the holder of the offline Ed25519 key and its passphrase. The key is made and
  used by `scripts/update-key.mjs` at a terminal (no pipe, variable or argument), stored encrypted (scrypt +
  AES-256-GCM, owner-only file, refused inside the project), never in CI. The public key is compiled in
  (`update-keys.txt`); several lines allow a planned rotation.
* **What is believed:** the manifest's signature is verified strictly (`verify_strict`, a context prefix) before the
  manifest is parsed; then product, schema, strict version, and each package (known system and kind, safe name,
  address exactly in the release folder, size, SHA-256). **No downgrade** and **no replay** (never older than a
  manifest already accepted, remembered across restarts).
* **What is kept and run:** the package goes to the app's own data folder, never past its signed size, keeps its name
  only after size + hash match, is checked again when the user presses Install, and once more by the installer step:
  on Windows under a read-only-sharing handle held until the setup started; the AppImage is replaced by exactly the
  bytes that were hashed (atomic rename); `.deb` / `.rpm` / `.dmg` open in the system's own installer. Nothing is
  installed without the user's click; a failure leaves the installed version untouched.
* **What is sent:** two small public files, then the package on request; no cookies, credentials, extra headers or
  identifiers (`VeloxFetch/<version>` only). Automatic checks can be turned off (`docs/PRIVACY.md`).
* **The UI's part:** six main-window commands, none taking an address, a path or a version; error texts come from
  fixed codes, never from the network.
* Threat 14 in `docs/threat-model.md`; design, rotation and recovery in `docs/auto-update.md`.

## Performance work (Phase 20, October 2026)

* Segment hashes (resume integrity) are now built checkpoint by checkpoint and kept for the run instead of read again
  at the end. Same rule as before: a hash is only made from bytes synced by that checkpoint, and is saved only for a
  complete segment. A restart, progress going back, a new plan or a changed range drops the running hashes, so a
  rewritten byte is always read. Tests: `verify` unit tests, `reverify.rs` (saved hashes equal the server's bytes).
* `tests/bench` is a test tool: it is not part of any package, and its server listens on 127.0.0.1 only for the
  length of a run.

## Release candidate 1.0.0 (Phase 21, October 2026)

* Security gate (`node scripts/security-gate.mjs`): formatting, clippy, 44 policy tests, `cargo audit` (0 of 614
  crates), `cargo deny`, `npm audit` (UI and extension: 0), lockfiles: passed. `cargo machete`: no unused dependency.
* Release builds ignore every `VELOX_*` test switch (data folder, dry runs, the update test feed, debug port).
* The UI now translates Rust's refusals and timeline lines by matching known English texts; an unknown text is shown
  as it came (never interpreted), and nothing of it reaches a command line, a path or the network.
* Review summary, tampering, and the owner's signing steps: `docs/release-candidate.md`.

## Publishing the release (October 2026)

* **Where users get it:** the public repository `IlyesBs/VeloxFetch-releases` only (the source repository is
  private). The app's feed, release page and every package address in the signed manifest name it; a policy test
  keeps the app, `update-key.mjs` and `publish-release.sh` equal, so a manifest can never list an address the app
  refuses, or one elsewhere.
* **Nothing unchecked is published** (`scripts/publish-kit.mjs`): each system's packages are checked against their
  own `SHA256SUMS.txt` before they are copied (a file damaged or changed on its way from the Windows or macOS
  computer is refused); only known file names of this version; two different files under one name refused; the
  built app must carry the update key and be a release build; the full security gate runs with every build.
* **The key:** typed at a terminal only (the script refuses to run without one; nothing passes a passphrase, and the
  kit reads no environment variable); the key file must be outside the repository and be one the app trusts. The
  release folder never holds the passphrase or the private key (tested).
* **GitHub:** the script at most creates a **draft** (`--github-draft`); it never publishes, edits, deletes, pushes or
  tags. The owner checks the draft and publishes it.
* **The pictures** come from the real app through a local proxy and example names: no address of the computer, no
  real site, no personal file.
